//! Medium, hermetic cutover corpus. Synthetic host publications exercise the
//! persisted read surfaces without native confinement or captured user state.
//! Transition behaviour is covered separately by graph_parity/graph_provider.
mod support;
use provenant_core::{
    AttemptId, CancelToken, Clock, Digest, OperationId, RootInputs, RunId, TaskId, WallTime,
};
use provenant_fabric_runtime::graph::{FileJournal, GraphDocument, GraphExecution, GraphJournal};
use provenant_os::{clock::SystemClock, exec::Termination};
use provenant_records::{CanonicalV1, run::RouteRecord};
use provenant_run::{
    CanonicalRequest, ExecutionContext, NewAttempt, PreparedRun, Request, RunStore,
};
use provenant_supervisor::custody::{
    CaptureTotals, ExitObservation, PublicationState, Scope, SettlementEvidence,
};
use serde_json::{Value, json};
use std::{ffi::OsString, fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path};
use support::{Result, TestRig, code};

const SCENARIOS: &[(&str, &[provenant_run::Outcome])] = &[
    ("success", &[provenant_run::Outcome::Succeeded]),
    ("failure", &[provenant_run::Outcome::Failed]),
    (
        "quota-fallback",
        &[
            provenant_run::Outcome::QuotaLimited,
            provenant_run::Outcome::Succeeded,
        ],
    ),
    (
        "writer-continuation",
        &[
            provenant_run::Outcome::QuotaLimited,
            provenant_run::Outcome::Succeeded,
        ],
    ),
    ("cancel", &[provenant_run::Outcome::Cancelled]),
    ("timeout", &[provenant_run::Outcome::TimedOut]),
    ("delivery", &[provenant_run::Outcome::Succeeded]),
    ("peer", &[provenant_run::Outcome::Succeeded]),
    ("nested", &[provenant_run::Outcome::Succeeded]),
];

fn write_sealed_fixture(path: &Path, bytes: &[u8]) -> Result {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)?;
    Ok(())
}

fn historical(rig: &TestRig, binary: &Path, args: &[&str]) -> Result<Value> {
    let mut arguments = vec![
        OsString::from("--state-dir"),
        rig.state.as_os_str().to_owned(),
    ];
    arguments.extend(args.iter().map(OsString::from));
    let output = rig.execute_os(binary, &arguments, None)?;
    Ok(json!({"exit":code(&output),"reply":serde_json::from_slice::<Value>(&output.stdout.bytes)?}))
}

fn seed(rig: &TestRig, name: &str, outcomes: &[provenant_run::Outcome]) -> Result<Vec<AttemptId>> {
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?;
    let context = provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)
        .map_err(|e| format!("open context: {e:?}"))?;
    let clock = SystemClock::new();
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let store = RunStore::open(
        &ExecutionContext {
            state_root: roots.state.clone(),
            project_id: context.identity.project,
            actor_id: context.identity.inbox.actor,
            read_prompts: true,
        },
        &call,
    )?
    .with_host(context.identity.host);
    let time = WallTime::from_micros(1_700_000_000_000_000)?;
    let route: RouteRecord = serde_json::from_value(json!({
        "provider":"fixture", "model":"fixture/model", "effort":null,
        "account_label":null,"account_key":null,"source":"fixture"
    }))?;
    let mut attempts = Vec::new();
    let mut graph_attempts = Vec::new();
    let chair = json!({"host":context.identity.host,"actor":context.identity.inbox.actor,"session":"fixture-chair"});
    let parent = AttemptId::from_bytes([50; 16]);
    if name == "nested" {
        let parent_task = TaskId::from_bytes([51; 16]);
        store.create(&PreparedRun {
            run_id:RunId::from_bytes([52;16]),operation_id:OperationId::parse("cutover-parent")?,created_at:time,
            request:serde_json::from_value(json!({"schema_version":1,"tasks":[{
                "task_id":parent_task,"prompt_digest":CanonicalV1::file_digest(b"Synthetic parent"),
                "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
                "fallback":[],"authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,"delivery_id":null
            }],"concurrency":1,"parent":null,"chair":chair}))?,
            attempts:vec![NewAttempt {task_id:parent_task,attempt_id:parent,prompt:"Synthetic parent".into()}],
        },&call)?;
        store.mark_starting_for_fixture(parent, &route, &call)?;
    }
    for (index, outcome) in outcomes.iter().copied().enumerate() {
        let ordinal = u8::try_from(index + 1)?;
        let run_id = RunId::from_bytes([ordinal; 16]);
        let attempt = AttemptId::from_bytes([ordinal + 16; 16]);
        let task = TaskId::from_bytes([ordinal + 32; 16]);
        let mut work = json!({"graph_id":name,"node_id":"work"});
        if name == "writer-continuation" && index > 0 {
            work["writer_predecessor"] = json!(attempts[0]);
        }
        if name == "nested" {
            work["parent_node"] = json!("parent");
        }
        let request: CanonicalRequest = serde_json::from_value(json!({
            "schema_version":1,"tasks":[{
                "task_id":task,"prompt_digest":CanonicalV1::file_digest(b"Synthetic cutover work"),
                "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
                "fallback":[],
                "authority_digest":Digest::from_bytes([5;32]),"provider_timeout_seconds":30,
                "delivery_id":null,"context":work
            }],"concurrency":1,"parent":null,
            "lane_depth":if name == "nested" {Some(1)} else {None},
            "parent_attempt":if name == "nested" {Some(parent)} else {None},
            "chair":chair,
        }))?;
        store.create(
            &PreparedRun {
                run_id,
                operation_id: OperationId::parse(&format!("cutover-{name}-{index}"))?,
                created_at: time,
                request,
                attempts: vec![NewAttempt {
                    task_id: task,
                    attempt_id: attempt,
                    prompt: "Synthetic cutover work".into(),
                }],
            },
            &call,
        )?;
        let starting = store
            .mark_starting_for_fixture(attempt, &route, &call)
            .map_err(|e| format!("mark starting: {e:?}"))?;
        let directory = rig
            .state
            .join("runs")
            .join(run_id.to_string())
            .join("attempts")
            .join(attempt.to_string());
        let exit = ExitObservation {
            publication: PublicationState::Durable,
            scope: Scope {
                run: run_id,
                attempt,
                generation: starting.generation,
            },
            termination: Some(Termination::Exit(
                if outcome == provenant_run::Outcome::Succeeded {
                    0
                } else {
                    1
                },
            )),
            never_spawned: false,
            launch_refusal: None,
            stop_reason: if outcome == provenant_run::Outcome::TimedOut {
                Some(provenant_supervisor::machine::StopReason::WallDeadline)
            } else {
                None
            },
            delivered_signals: vec![],
            reaped: vec![],
            omitted_reaped: 0,
            spared: vec![],
            omitted_spared: 0,
            cancel_delivered_before_exit: (outcome == provenant_run::Outcome::Cancelled)
                .then_some(true),
            warnings: vec![],
            omitted_warnings: 0,
            terminal_observed: true,
            unresolved_cleanup: false,
            unresolved_pids: vec![],
            settlement_evidence: SettlementEvidence::DarwinCensusAndCustodyLease,
            stdout: CaptureTotals::default(),
            stderr: CaptureTotals::default(),
        };
        let mut sealed = serde_json::to_value(&exit)?;
        sealed
            .as_object_mut()
            .ok_or("sealed exit object")?
            .remove("publication");
        sealed["schema_version"] = json!(1);
        write_sealed_fixture(
            &directory.join("process-exit.json"),
            &serde_json::to_vec(&sealed)?,
        )?;
        if name == "writer-continuation" || name == "delivery" {
            write_sealed_fixture(
                &directory.join("writer-terminal.json"),
                &serde_json::to_vec(&json!({
                    "schema_version":1,"head":"2".repeat(40),"base":"1".repeat(40)
                }))?,
            )?;
        }
        if name == "delivery" {
            write_sealed_fixture(
                &directory.join("delivery.json"),
                &serde_json::to_vec(&json!({
                    "schema_version":1,"remote":"origin","branch":"fixture/delivery","head":"2".repeat(40),
                    "state":{"state":"delivered","pull_request":null}
                }))?,
            )?;
        }
        let result = format!("Synthetic {name} attempt {index}");
        let terminal = store.publish_terminal(
            attempt,
            starting.revision,
            &provenant_run::TerminalPublication {
                terminal_cause: None,
                terminal_note: None,
                outcome,
                finished_at: time,
                started_at: Some(time),
                provider_ms: Some(1),
                reason: None,
                result: &result,
                complete: true,
                redacted: false,
                resolved_route: Some(route.clone()),
                applied_route: Some(route.clone()),
                observed_route: Some(route.clone()),
                usage: Default::default(),
                session: None,
                partial_signals: vec![],
            },
            &exit,
            &call,
        )?;
        assert_eq!(terminal.trace.host, Some(context.identity.host));
        let sealed_request = store.request(run_id, &call)?;
        assert_eq!(serde_json::to_value(&sealed_request.chair)?, chair);
        if name == "nested" {
            assert_eq!(sealed_request.parent_attempt, Some(parent));
            assert_eq!(sealed_request.lane_depth, Some(1));
            assert_eq!(terminal.trace.parent, Some(parent));
            assert_eq!(
                terminal
                    .context
                    .as_ref()
                    .ok_or("nested work context")?
                    .parent_node
                    .as_deref(),
                Some("parent")
            );
        }
        if name == "writer-continuation" {
            assert_eq!(
                terminal.head.map(|head| head.to_string()),
                Some("2".repeat(40))
            );
            assert_eq!(
                terminal
                    .context
                    .as_ref()
                    .ok_or("writer work context")?
                    .writer_predecessor,
                attempts.first().copied()
            );
        }
        if name == "delivery" {
            assert!(matches!(
                terminal.delivery.as_ref().map(|receipt| &receipt.state),
                Some(provenant_records::run::WriterDeliveryState::Delivered { .. })
            ));
        }
        let reference = format!("{}/{attempt}", context.identity.host);
        graph_attempts.push(json!({
            "launched":true,"key":{"graph_id":name,"node_id":"work","route_index":index},
            "route":{"adapter":"fixture","model":"fixture/model"},"state":"terminal",
            "attempt_ref":reference,"outcome":match outcome {
                provenant_run::Outcome::Succeeded => "succeeded",
                provenant_run::Outcome::QuotaLimited => "usage_limited",
                provenant_run::Outcome::TimedOut => "deadline_exceeded",
                _ => "task_failed"
            },"launch_actor":context.identity.inbox.actor.to_string()
        }));
        attempts.push(attempt);
    }
    let routes: Vec<_> = outcomes
        .iter()
        .map(|_| json!({"adapter":"fixture","model":"fixture/model"}))
        .collect();
    let document: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"graph_id":name,"nodes":[{"id":"work","prompt":"Synthetic cutover work",
        "access":"read_only","routes":routes}]
    }))?;
    let mut journal = FileJournal::open(
        &roots.state,
        context.identity.project,
        name,
        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
        &cancel,
        &clock,
    )?;
    let origin = json!({"graph_id":"origin-graph","node_id":"origin-node","tier":"senior","template":"task"});
    journal.bind(GraphExecution {
        extra: if name == "peer" {
            [("peer_origin_context".into(), origin.clone())].into()
        } else {
            Default::default()
        },
        integration_base: None,
        review_base: None,
        directory: rig.project.clone(),
        document: document.clone(),
    })?;
    let mut checkpoint = provenant_fabric_runtime::graph::advance_with_budget(
        &document,
        &mut NoLaunch,
        &mut journal,
        &Default::default(),
        &Default::default(),
    )?
    .checkpoint;
    let node = &mut checkpoint.nodes[0];
    node.state = if outcomes.last() == Some(&provenant_run::Outcome::Succeeded) {
        provenant_fabric_runtime::graph::NodeState::Succeeded
    } else {
        provenant_fabric_runtime::graph::NodeState::Failed
    };
    node.attempts = serde_json::from_value(json!(graph_attempts))?;
    journal
        .save(&checkpoint)
        .map_err(|e| format!("save graph: {e:?}"))?;
    if name == "peer" {
        assert_eq!(
            FileJournal::execution(&roots.state, context.identity.project, name)?
                .ok_or("peer execution")?
                .extra["peer_origin_context"],
            origin
        );
    }
    Ok(attempts)
}

struct NoLaunch;
impl provenant_fabric_runtime::graph::AttemptOwner for NoLaunch {
    type Error = std::convert::Infallible;
    fn actor(&self) -> String {
        "fixture".into()
    }
    fn observe_or_start(
        &mut self,
        _: &provenant_fabric_runtime::graph::AttemptKey,
        _: provenant_fabric_runtime::graph::NodeInput<'_>,
        _: &provenant_fabric_runtime::graph::GraphRoute,
        _: provenant_fabric_runtime::graph::AccessMode,
        _: Option<&str>,
        _: provenant_fabric_runtime::graph::Replay<'_>,
    ) -> std::result::Result<provenant_fabric_runtime::graph::AttemptObservation, Self::Error> {
        Ok(provenant_fabric_runtime::graph::AttemptObservation::Unknown { attempt_ref: None })
    }
}

#[test]
fn generated_cutover_status_and_output_corpus() -> Result {
    let baseline = std::env::var_os("PROVENANT_GRAPH_CUTOVER_BASELINE");
    let frozen: Value =
        serde_json::from_slice(include_bytes!("fixtures/graph-cutover/read-baseline.json"))?;
    let mut capture = serde_json::Map::new();
    for (name, outcomes) in SCENARIOS {
        let rig = TestRig::new()?;
        let attempts = seed(&rig, name, outcomes).map_err(|e| format!("seed {name}: {e:?}"))?;
        let mut captured = Vec::new();
        for (index, attempt) in attempts.into_iter().enumerate() {
            let id = attempt.to_string();
            let mut entry = serde_json::Map::new();
            for args in [
                vec!["lanes", "--id", &id, "--all", "--json"],
                vec!["status", &id, "--json"],
                vec!["output", &id, "--json"],
            ] {
                let old = match baseline.as_deref() {
                    Some(binary) => normalise(historical(&rig, Path::new(binary), &args)?),
                    None => frozen[name][index][args[0]].clone(),
                };
                assert_eq!(
                    old, frozen[name][index][args[0]],
                    "historical capture drift: {name} {args:?}"
                );
                let new_args = if args[0] == "lanes" {
                    vec!["status", &id, "--json"]
                } else {
                    args.clone()
                };
                let output = rig.command(&new_args)?;
                assert_eq!(
                    json!(code(&output)),
                    old["exit"],
                    "exit code: {name} {new_args:?}"
                );
                let new: Value = normalise(serde_json::from_slice(&output.stdout.bytes)?);
                assert_eq!(new["status"], old["reply"]["status"]);
                assert_eq!(new["error"], old["reply"]["error"]);
                if args[0] == "output" {
                    assert_eq!(
                        new["data"], old["reply"]["data"],
                        "output fields: {name} {id}"
                    );
                } else {
                    let mut row = new["data"]["rows"]
                        .as_array()
                        .ok_or("unified rows")?
                        .iter()
                        .find(|row| {
                            row["kind"] == "attempt"
                                && row["attempt_id"]
                                    .as_str()
                                    .is_some_and(|reference| reference.ends_with(&format!("/{id}")))
                        })
                        .ok_or("unified attempt row")?
                        .clone();
                    assert_eq!(row["id"], row["attempt_id"]);
                    assert_eq!(row["graph_id"], *name);
                    assert_eq!(row["node_id"], "work");
                    assert_eq!(row["graph_state"], "terminal");
                    row["attempt_id"] = json!(id);
                    for field in [
                        "id",
                        "kind",
                        "graph_id",
                        "node_id",
                        "graph_state",
                        "cause_code",
                        "outcome",
                        "next",
                    ] {
                        assert!(
                            row.as_object_mut().unwrap().remove(field).is_some(),
                            "missing added field {field}"
                        );
                    }
                    let mut before = old["reply"]["data"]["lanes"][0].clone();
                    if args[0] == "lanes" {
                        assert_eq!(
                            before.as_object_mut().unwrap().remove("handoff"),
                            Some(Value::Null),
                            "a nonempty legacy policy handoff needs graph-lineage evidence"
                        );
                    }
                    assert_eq!(row, before, "status fields: {name} {id} via {}", args[0]);
                }
                entry.insert(args[0].into(), old);
            }
            captured.push(Value::Object(entry));
        }
        capture.insert((*name).into(), Value::Array(captured));
    }
    if let Some(directory) = std::env::var_os("PROVENANT_GRAPH_CUTOVER_CAPTURE") {
        fs::create_dir_all(&directory)?;
        fs::write(
            Path::new(&directory).join("read-baseline.json"),
            serde_json::to_vec(&capture)?,
        )?;
    }
    Ok(())
}

/// Run IDs, outcomes, result bytes, digests and exit codes remain exact. Only
/// the generated project identity and scratch path prefix vary per fixture.
fn normalise(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            for (key, field) in object {
                if key == "project_id" {
                    *field = json!("synthetic-project");
                } else {
                    *field = normalise(field.take());
                }
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| *item = normalise(item.take())),
        Value::String(text) => {
            if let Some((_, tail)) = text.split_once("/state/runs/") {
                *text = format!("synthetic/graph-cutover/state/runs/{tail}");
            }
        }
        _ => {}
    }
    value
}

/// The checkpoint payload was written by the pinned pre-cut binary. Its
/// serializer is deliberately independent of today's GraphCheckpoint type.
#[test]
fn pre_cut_binary_terminal_checkpoint_remains_readable() -> Result {
    let rig = TestRig::new()?;
    let mut inputs = RootInputs::new(rig.home.clone(), rig.project.clone());
    inputs.state_dir = Some(rig.state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let project = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(rig.project.join(".git"))?,
    )?
    .project_id();
    provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?;
    let fixture = include_bytes!("fixtures/graph-cutover/old-terminal-checkpoint.json");
    let raw: Value = serde_json::from_slice(fixture)?;
    let provenance: Value =
        serde_json::from_slice(include_bytes!("fixtures/graph-cutover/provenance.json"))?;
    assert_eq!(
        CanonicalV1::file_digest(CanonicalV1::bytes(&raw["checkpoint"])?.as_bytes()).to_string(),
        format!(
            "sha256:{}",
            provenance["checkpoint_payload_sha256"].as_str().unwrap()
        )
    );
    let clock = SystemClock::new();
    let journal = FileJournal::open(
        &roots.state,
        project,
        "old-terminal-fixture",
        provenant_core::WaitBudget::nonblocking(clock.mono_now()),
        &CancelToken::new(),
        &clock,
    )?;
    drop(journal);
    fs::write(
        rig.state
            .join("fabric/graphs")
            .join(project.to_string())
            .join("graph-old-terminal-fixture.json"),
        fixture,
    )?;
    let checkpoint = FileJournal::read_checkpoint(&roots.state, project, "old-terminal-fixture")?
        .ok_or("checkpoint")?;
    assert!(checkpoint.successful());
    assert!(
        checkpoint.nodes[0].attempts.is_empty(),
        "a deterministic gate starts no provider"
    );
    let output = rig.command(&["status", "old-terminal-fixture", "--json"])?;
    assert_eq!(code(&output), 0, "{output:?}");
    let reply: Value = serde_json::from_slice(&output.stdout.bytes)?;
    let rows = reply["data"]["rows"].as_array().ok_or("status rows")?;
    assert!(
        rows.iter().any(|row| row["kind"] == "graph"
            && row["state"] == "complete"
            && row["outcome"] == "succeeded"),
        "{reply}"
    );
    assert!(
        rows.iter()
            .any(|row| row["kind"] == "node" && row["state"] == "succeeded"),
        "{reply}"
    );
    Ok(())
}
