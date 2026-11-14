//! `wait` at the binary boundary: targets resolve before any wait, a settled
//! graph ends the wait with its verdict, and no work means nothing to wait for.
mod support;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Read},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use support::{Result, TestRig, code};

fn json_lines(bytes: &[u8]) -> Result<Vec<Value>> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .map(serde_json::from_slice::<Value>)
        .collect::<std::result::Result<_, _>>()
        .map_err(Into::into)
}

fn records(output: &provenant_os::exec::ExecOutcome) -> Result<Vec<Value>> {
    json_lines(&output.stdout.bytes)
}

fn envelope(output: &provenant_os::exec::ExecOutcome) -> Result<Value> {
    records(output)?
        .pop()
        .ok_or_else(|| "command emitted no JSON records".into())
}

fn assert_digest_fields(digest: &Value) {
    for field in [
        "key",
        "outcome",
        "cause",
        "branch",
        "pr",
        "ci",
        "result_path",
        "provider_error",
        "line",
        "handoff_status",
        "open_asks",
        "settled",
    ] {
        assert!(digest.get(field).is_some(), "missing {field}: {digest}");
    }
    assert!(digest["key"]["project"].is_string(), "{digest}");
    assert!(digest["key"]["graph"].is_string(), "{digest}");
    assert!(digest["key"]["node"].is_string(), "{digest}");
    for field in ["pr", "ci"] {
        assert!(
            matches!(digest[field]["state"].as_str(), Some("unknown" | "stale")),
            "{field} state is unlabelled: {digest}"
        );
    }
}

#[allow(clippy::disallowed_methods, reason = "CLI boundary fixture owns child process I/O")]
fn fixture_cli(rig: &TestRig, args: &[&str]) -> Result<Output> {
    let binary = option_env!("CARGO_BIN_EXE_provenant")
        .ok_or("production binary path is only available to integration targets")?;
    let state = rig.state.to_str().ok_or("state path is not UTF-8")?;
    Ok(Command::new(binary)
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .current_dir(&rig.project)
        .envs(rig.env.iter().cloned())
        .output()?)
}

fn graph_identity(rig: &TestRig) -> Result<(provenant_core::StateRoot, provenant_core::ProjectId)> {
    let mut inputs = provenant_core::RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let identity = rig.command(&["fabric", "identity", "--json"])?;
    let identity: Value = serde_json::from_slice(&identity.stdout.bytes)?;
    let project_id = identity["data"]["project"]
        .as_str()
        .ok_or("project identity")?;
    let project = provenant_core::ProjectId::parse(project_id)?;
    Ok((roots.state, project))
}

fn graph_journal(
    graph: &str,
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    clock: &provenant_os::clock::SystemClock,
    cancel: &provenant_core::CancelToken,
) -> Result<provenant_fabric_runtime::graph::FileJournal> {
    use provenant_core::Clock;
    let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 30)?;
    Ok(provenant_fabric_runtime::graph::FileJournal::open(
        state, project, graph, budget, cancel, clock,
    )?)
}

fn seed_graph(rig: &TestRig, graph: &str) -> Result<()> {
    use provenant_fabric_runtime::graph::{GraphDocument, GraphExecution, GraphJournal};
    let (state, project) = graph_identity(rig)?;
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":graph,"nodes":[
            {"id":"fast","kind":"gate","access":"read_only","condition":{"type":"budget"}},
            {"id":"slow","kind":"gate","access":"read_only","condition":{"type":"budget"}},
            {"id":"blocked","prompt":"blocked writer","access":"worktree_write","write_scope":["."]},
            {"id":"skipped","kind":"gate","access":"read_only","condition":{"type":"budget"}}
        ]
    }))?;
    let checkpoint: provenant_fabric_runtime::graph::GraphCheckpoint =
        serde_json::from_value(json!({
            "schema_version":1,"graph_id":graph,"plan_digest":format!("sha256:{}", "f".repeat(64)),
            "nodes":[
                {"id":"fast","state":"pending","attempts":[]},
                {"id":"slow","state":"pending","attempts":[]},
                {"id":"blocked","state":"pending","attempts":[]},
                {"id":"skipped","state":"pending","attempts":[]}
            ]
        }))?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let mut journal = graph_journal(graph, &state, project, &clock, &cancel)?;
    journal.bind(GraphExecution {
        review_base: None,
        extra: Default::default(),
        integration_base: None,
        directory: rig.project.clone(),
        document,
    })?;
    journal.save(&checkpoint)?;
    Ok(())
}

fn set_node_states(rig: &TestRig, graph: &str, changes: &[(&str, &str)]) -> Result<()> {
    use provenant_fabric_runtime::graph::GraphJournal;
    let (state, project) = graph_identity(rig)?;
    let clock = provenant_os::clock::SystemClock::new();
    let cancel = provenant_core::CancelToken::new();
    let mut journal = graph_journal(graph, &state, project, &clock, &cancel)?;
    let mut checkpoint = journal.load(graph)?.ok_or("graph checkpoint")?;
    for (id, state) in changes {
        let node = checkpoint
            .nodes
            .iter_mut()
            .find(|node| node.id == *id)
            .ok_or("graph node")?;
        node.state = serde_json::from_value(json!(state))?;
        if *state == "blocked" {
            node.blocked = true;
            node.handoff = Some(serde_json::from_value(json!({
                "schema_version":2,"status":"blocked","note":"Waiting for an owner decision.",
                "remaining":["Owner decision"],"decisions":[],"verify":[]
            }))?);
        }
    }
    journal.save(&checkpoint)?;
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Asserts the real wait deadline is not consumed.
fn an_unknown_target_fails_before_waiting() -> Result {
    let rig = TestRig::new()?;
    // A budget far beyond any process start-up: returning well inside it shows
    // the target was refused before the wait began, however slow the host.
    let budget = Duration::from_secs(3600);
    let started = Instant::now();
    let out = rig.command(&["wait", "no-such-graph", "--timeout", "1h", "--json"])?;
    assert_eq!(code(&out), 2, "{out:?}");
    assert_eq!(envelope(&out)?["error"]["code"], "NOT_FOUND");
    assert!(started.elapsed() < budget / 4, "{:?}", started.elapsed());
    Ok(())
}

#[test]
fn a_settled_graph_ends_the_wait_and_no_work_is_nothing_to_wait_for() -> Result {
    let rig = TestRig::new()?;
    let idle = rig.command(&["wait", "--timeout", "30s", "--json"])?;
    assert_eq!(code(&idle), 0, "{idle:?}");
    assert_eq!(envelope(&idle)?["data"]["targets"], json!([]));
    let document = rig.root.join("graph.json");
    std::fs::write(
        &document,
        serde_json::to_vec(&json!({
            "schema_version":1,"graph_id":"settles-now","nodes":[{
                "id":"accept","kind":"gate","access":"read_only","condition":{"type":"budget"}
            }]
        }))?,
    )?;
    let run = rig.command(&[
        "run",
        "--file",
        document.to_str().ok_or("document path")?,
        "--json",
    ])?;
    assert_eq!(code(&run), 0, "{run:?}");
    let out = rig.command(&["wait", "settles-now", "--timeout", "30s", "--json"])?;
    let value = envelope(&out)?;
    assert_eq!(code(&out), 0, "{value}");
    assert_eq!(value["data"]["timed_out"], false, "{value}");
    assert_eq!(value["data"]["targets"][0]["target"], "settles-now");
    assert_eq!(value["data"]["targets"][0]["settle"], "succeeded");
    let digests = value["data"]["targets"][0]["data"]["digests"]
        .as_array()
        .ok_or("wait node digests")?;
    assert!(!digests.is_empty(), "{value}");
    for digest in digests {
        assert_digest_fields(digest);
    }
    let events = records(&out)?;
    assert!(
        events[..events.len() - 1]
            .iter()
            .any(|record| record["event"] == "settled" && record["target"] == "settles-now")
    );

    let status = rig.command(&["status", "settles-now", "--json"])?;
    assert_eq!(code(&status), 0, "{status:?}");
    let status_value = envelope(&status)?;
    let digests = status_value["data"]["digests"]
        .as_array()
        .ok_or("status node digests")?;
    assert!(!digests.is_empty(), "{status_value}");
    for digest in digests {
        assert_digest_fields(digest);
    }
    Ok(())
}

#[test]
fn status_omissions_are_counted_per_state_and_paged() -> Result {
    let rig = TestRig::new()?;
    for (graph, terminal_state) in [
        ("first-page", "succeeded"),
        ("failed-page", "failed"),
        ("third-page", "succeeded"),
    ] {
        seed_graph(&rig, graph)?;
        let changes = [
            ("fast", terminal_state),
            ("slow", terminal_state),
            ("blocked", terminal_state),
            ("skipped", terminal_state),
        ];
        set_node_states(&rig, graph, &changes)?;
    }

    let first = rig.command(&["status", "--all", "--limit", "1", "--json"])?;
    assert_eq!(code(&first), 0, "{first:?}");
    let first = envelope(&first)?;
    assert_eq!(first["data"]["graphs"].as_array().map(Vec::len), Some(1));
    let shown = first["data"]["graphs"][0]["state"]
        .as_str()
        .ok_or("shown graph state")?;
    let omitted = &first["data"]["omitted"];
    let expected_succeeded = 2 - usize::from(shown == "succeeded");
    let expected_failed = 1 - usize::from(shown == "failed");
    assert_eq!(
        omitted["succeeded"].as_u64().unwrap_or(0),
        expected_succeeded as u64,
        "{first}"
    );
    assert_eq!(
        omitted["failed"].as_u64().unwrap_or(0),
        expected_failed as u64,
        "{first}"
    );
    assert_eq!(first["data"]["next_cursor"], 1, "{first}");

    let second = rig.command(&["status", "--all", "--limit", "1", "--cursor", "1", "--json"])?;
    assert_eq!(code(&second), 0, "{second:?}");
    let second = envelope(&second)?;
    assert_eq!(second["data"]["graphs"].as_array().map(Vec::len), Some(1));
    assert_ne!(
        second["data"]["graphs"][0]["name"],
        first["data"]["graphs"][0]["name"]
    );
    assert_eq!(second["data"]["next_cursor"], 2, "{second}");
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods)] // Bound asynchronous CLI state transitions.
fn wait_any_wakes_on_node_settlement_and_wait_reports_early_events() -> Result {
    let rig = TestRig::new()?;
    let graph = "wait-transition";
    seed_graph(&rig, graph)?;
    let binary = option_env!("CARGO_BIN_EXE_provenant")
        .ok_or("production binary path is only available to integration targets")?;
    let mut wait_any = Command::new(binary)
        .arg("--state-dir")
        .arg(&rig.state)
        .args(["wait", graph, "--any", "--follow", "--timeout", "30s", "--json"])
        .current_dir(&rig.project)
        .envs(rig.env.iter().cloned())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    // The first follow record is an acknowledgement of the initial snapshot,
    // so journal changes cannot race a slow process start on a loaded host.
    let mut any_stdout = std::io::BufReader::new(wait_any.stdout.take().ok_or("any wait stdout")?);
    let mut initial = String::new();
    assert!(any_stdout.read_line(&mut initial)? > 0, "missing initial snapshot");
    let initial: Value = serde_json::from_str(&initial)?;
    assert_eq!(initial["event"], "transition", "{initial}");
    set_node_states(&rig, graph, &[("slow", "running")])?;
    let mut running = String::new();
    assert!(any_stdout.read_line(&mut running)? > 0, "missing running snapshot");
    let running: Value = serde_json::from_str(&running)?;
    assert_eq!(running["event"], "transition", "progress did not settle: {running}");
    assert!(running["digests"].as_array().ok_or("running digests")?.iter()
        .any(|digest| digest["key"]["node"] == "slow" && digest["state"] == "running"), "{running}");
    assert!(wait_any.try_wait()?.is_none(), "running is not settled");
    set_node_states(&rig, graph, &[("fast", "succeeded")])?;
    let mut settled_records = String::new();
    any_stdout.read_to_string(&mut settled_records)?;
    let any_status = wait_any.wait()?;
    assert_eq!(any_status.code(), Some(0), "{settled_records}");
    let any_records = json_lines(settled_records.as_bytes())?;
    assert!(
        any_records[..any_records.len() - 1]
            .iter()
            .any(|record| record["event"] == "node_settled" && record["target"] == graph)
    );
    let any_result = any_records.last().ok_or("final any wait record")?;
    assert_eq!(any_result["data"]["timed_out"], false, "{any_result}");
    let graph_status = fixture_cli(&rig, &["status", graph, "--json"])?;
    let graph_status: Value = serde_json::from_slice(&graph_status.stdout)?;
    assert_eq!(
        graph_status["data"]["rows"][0]["state"], "pending",
        "{graph_status}"
    );

    let fast = format!("{graph}/fast");
    let slow = format!("{graph}/slow");
    let mut wait_all = Command::new(binary)
        .arg("--state-dir")
        .arg(&rig.state)
        .args(["wait", &fast, &slow, "--timeout", "30s", "--json"])
        .current_dir(&rig.project)
        .envs(rig.env.iter().cloned())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let wait_stdout = wait_all.stdout.take().ok_or("wait stdout")?;
    let mut wait_lines = std::io::BufReader::new(wait_stdout);
    let mut event_line = String::new();
    assert!(
        wait_lines.read_line(&mut event_line)? > 0,
        "missing early event"
    );
    let early: Value = serde_json::from_str(&event_line)?;
    assert_eq!(early["event"], "settled", "{early}");
    assert_eq!(early["target"], fast, "{early}");
    for field in ["settle", "line", "digests"] {
        assert!(early.get(field).is_some(), "missing {field}: {early}");
    }
    let early_digests = early["digests"].as_array().ok_or("early event digests")?;
    assert!(!early_digests.is_empty(), "{early}");
    for digest in early_digests {
        assert_digest_fields(digest);
    }
    assert!(
        wait_all.try_wait()?.is_none(),
        "wait ended while {slow} was pending"
    );

    set_node_states(
        &rig,
        graph,
        &[
            ("slow", "failed"),
            ("blocked", "blocked"),
            ("skipped", "skipped"),
        ],
    )?;
    let mut rest = String::new();
    wait_lines.read_to_string(&mut rest)?;
    let wait_status = wait_all.wait()?;
    assert_eq!(wait_status.code(), Some(1), "{rest}");
    let mut wait_records = vec![early];
    wait_records.extend(json_lines(rest.as_bytes())?);
    assert!(
        wait_records
            .iter()
            .any(|record| record["event"] == "settled" && record["target"] == slow)
    );
    let wait_result = wait_records.last().ok_or("final wait record")?;
    assert_eq!(
        wait_result["data"]["targets"].as_array().map(Vec::len),
        Some(2)
    );
    for target in wait_result["data"]["targets"].as_array().ok_or("targets")? {
        let digests = target["data"]["digests"].as_array().ok_or("wait digests")?;
        assert!(!digests.is_empty(), "{wait_result}");
        for digest in digests {
            assert_digest_fields(digest);
        }
    }

    for (node, state) in [
        ("fast", "succeeded"),
        ("slow", "failed"),
        ("blocked", "blocked"),
        ("skipped", "skipped"),
    ] {
        let target = format!("{graph}/{node}");
        let status = fixture_cli(&rig, &["status", &target, "--json"])?;
        assert!(status.status.success(), "{status:?}");
        let status = json_lines(&status.stdout)?.pop().ok_or("status envelope")?;
        assert_eq!(status["data"]["rows"][0]["state"], state, "{status}");
        let digests = status["data"]["digests"]
            .as_array()
            .ok_or("status digests")?;
        assert!(!digests.is_empty(), "{status}");
        for digest in digests {
            assert_digest_fields(digest);
        }
        if node == "blocked" {
            assert!(
                digests
                    .iter()
                    .any(|digest| digest["handoff_status"] == "blocked")
            );
        }

        let waiting = fixture_cli(&rig, &["wait", &target, "--any", "--json"])?;
        let waiting = json_lines(&waiting.stdout)?.pop().ok_or("wait envelope")?;
        let digests = waiting["data"]["targets"][0]["data"]["digests"]
            .as_array()
            .ok_or("wait digests")?;
        assert!(!digests.is_empty(), "{waiting}");
        for digest in digests {
            assert_digest_fields(digest);
        }
    }
    Ok(())
}
