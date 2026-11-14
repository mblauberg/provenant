//! Killed relay hosts recover children whose provider acceptance was journalled.
mod support;
use provenant_core::{AttemptId, CancelToken, Clock, RootInputs, WaitBudget};
use provenant_fabric_runtime::graph::{self, AttemptOwner, FileJournal, GraphExecution};
use provenant_os::clock::SystemClock;
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Child, Command},
    time::Duration,
};
use support::{Result, TestRig};
struct Provider {
    parent: String,
    recover: bool,
}
impl AttemptOwner for Provider {
    type Error = std::io::Error;
    fn actor(&self) -> String {
        "relay-fixture".into()
    }
    fn observe_or_start(
        &mut self,
        key: &graph::AttemptKey,
        _: graph::NodeInput<'_>,
        _: &graph::GraphRoute,
        _: graph::AccessMode,
        _: Option<&str>,
        replay: graph::Replay<'_>,
    ) -> std::io::Result<graph::AttemptObservation> {
        if key.node_id == "parent" {
            return Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some(self.parent.clone()),
            });
        }
        if self.recover {
            assert_eq!(replay.stored_ref, Some("fixture/accepted-child"));
            Ok(graph::AttemptObservation::Terminal {
                attempt_ref: replay.stored_ref.map(str::to_owned),
                outcome: graph::AttemptOutcome::Succeeded,
                launched: true,
                result: None,
                result_warning: None,
                result_path: None,
                branch: None,
                commit: None,
                stop: None,
            })
        } else {
            assert!(replay.stored_ref.is_none());
            Ok(graph::AttemptObservation::Pending {
                attempt_ref: Some("fixture/accepted-child".into()),
            })
        }
    }
}
fn context(home: &Path, project: &Path, state: &Path) -> Result<provenant_cli::fabric::Context> {
    let mut inputs = RootInputs::new(home.into(), project.into());
    inputs.state_dir = Some(state.into());
    Ok(provenant_cli::fabric::Context::open(
        inputs.resolve(&provenant_os::fs::SystemPathResolver)?,
        home,
        project,
    )?)
}
fn advance(
    context: &provenant_cli::fabric::Context,
    attempt: AttemptId,
    recover: bool,
    directory: &Path,
) -> Result {
    let clock = SystemClock::new();
    let mut journal = FileJournal::open(
        &context.roots.state,
        context.identity.project,
        "relay-crash",
        WaitBudget::cli(clock.mono_now(), 10)?,
        &CancelToken::new(),
        &clock,
    )?;
    let document: graph::GraphDocument = serde_json::from_value(
        json!({"schema_version":1,"graph_id":"relay-crash","template_library":{},"nodes":[{"id":"parent","access":"read_only","prompt":"Parent","route":{"alias":"senior"}}]}),
    )?;
    journal.bind(GraphExecution {
        document: document.clone(),
        directory: directory.to_path_buf(),
        review_base: None,
        integration_base: None,
        extra: Default::default(),
    })?;
    graph::advance_with_budget(
        &document,
        &mut Provider {
            parent: format!("{}/{attempt}", context.identity.host),
            recover,
        },
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?;
    Ok(())
}
#[test]
#[ignore = "subprocess helper invoked by relay_host_crash_recovers_in_flight_child"]
fn relay_host_process() -> Result {
    let root = std::env::var_os("RELAY_CRASH_ROOT").ok_or("missing helper root")?;
    let root = Path::new(&root);
    let context = context(
        &root.join("home"),
        &root.join("project"),
        &root.join("state"),
    )?;
    let attempt = AttemptId::parse(&std::env::var("RELAY_CRASH_ATTEMPT")?)?;
    let recover = std::env::var_os("RELAY_CRASH_RECOVER").is_some();
    let directory = root.join("project");
    advance(&context, attempt, recover, &directory)?;
    let marker = root.join("ready");
    let _relay = provenant_fabric_runtime::relay::HostRelay::start_with_graph(
        provenant_providers::LaneRuntime::fabric_socket(attempt),
        provenant_records::Limits::CONFIG,
        |_, _, _| unreachable!("coordination not requested"),
        move |arguments, cancel, within| {
            let clock = SystemClock::new();
            let value = provenant_cli::fabric::graph_relay::serve(
                &context,
                &attempt.to_string(),
                arguments,
                provenant_fabric_runtime::relay::host_budget(&clock, within)?,
                cancel,
                &clock,
            )?;
            advance(&context, attempt, recover, &directory).map_err(|error| {
                provenant_fabric_runtime::relay::ServiceError::Cli {
                    code: "fixture".into(),
                    message: error.to_string(),
                    data: Value::Null,
                }
            })?;
            Ok(value)
        },
    )?;
    std::fs::write(marker, "ready")?;
    loop {
        std::thread::park();
    }
}
struct SocketCleanup(std::path::PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}
struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
// Explicit process and real-time boundary: this test must SIGKILL a host without
// supervisor cleanup, then wait for its replacement to bind the retained socket.
#[allow(clippy::disallowed_methods)]
fn start(rig: &TestRig, attempt: AttemptId, recover: bool) -> Result<Host> {
    let ready = rig.root.join("ready");
    if ready.exists() {
        std::fs::remove_file(&ready)?;
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "relay_host_process", "--ignored", "--nocapture"])
        .envs(rig.env.iter().cloned())
        .env("RELAY_CRASH_ROOT", &rig.root)
        .env("RELAY_CRASH_ATTEMPT", attempt.to_string());
    if recover {
        command.env("RELAY_CRASH_RECOVER", "1");
    }
    let mut host = Host(command.spawn()?);
    let (ready, exited) = provenant_test_support::wait_for(
        "relay host readiness",
        Duration::from_secs(20),
        || (ready.exists(), host.0.try_wait()),
        |(ready, exited)| *ready || !matches!(exited, Ok(None)),
    )?;
    assert!(ready, "relay host exited before ready: {:?}", exited?);
    Ok(host)
}
#[test]
fn relay_host_crash_recovers_in_flight_child() -> Result {
    let rig = TestRig::new()?;
    let attempt = AttemptId::from_bytes(provenant_os::fs::entropy_128()?);
    let _socket = SocketCleanup(provenant_providers::LaneRuntime::fabric_socket(attempt));
    let host = start(&rig, attempt, false)?;
    let request = json!({"op":"add","id":"child","use":"task","brief":"Child work","idempotency_key":"child-once"});
    let send = |request| {
        provenant_fabric_runtime::relay::graph(
            &provenant_providers::LaneRuntime::fabric_socket(attempt),
            request,
            &CancelToken::new(),
            Duration::from_secs(10),
        )
    };
    let receipt = send(request.clone())?;
    let context = context(&rig.home, &rig.project, &rig.state)?;
    let read = || {
        FileJournal::read_checkpoint(
            &context.roots.state,
            context.identity.project,
            "relay-crash",
        )
    };
    let saved = read()?.ok_or("checkpoint")?;
    let child = saved
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("child")?;
    assert_eq!(child.state, graph::NodeState::Running);
    // SIGKILL leaves both the relay socket and accepted child behind.
    drop(host);
    assert!(provenant_providers::LaneRuntime::fabric_socket(attempt).exists());
    let _restarted = start(&rig, attempt, true)?;
    let replay = send(request)?;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["node"], receipt["node"]);
    let saved = read()?.ok_or("recovered checkpoint")?;
    let child = saved
        .nodes
        .iter()
        .find(|node| node.id == "parent--child")
        .ok_or("recovered child")?;
    assert_eq!(child.state, graph::NodeState::Succeeded);
    assert_eq!(child.attempts.len(), 1);
    assert_eq!(
        child.attempts[0].attempt_ref.as_deref(),
        Some("fixture/accepted-child")
    );
    Ok(())
}
