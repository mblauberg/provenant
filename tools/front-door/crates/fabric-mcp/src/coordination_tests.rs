//! The MCP dependency edge is runtime-only, including the real-store tests.
use crate::tools::*;
use provenant_core::{
    ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
    WaitBudget, WallTime,
};
use provenant_fabric_runtime::coordination::{CoordinationService, ThreadPause};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    error::Error,
    path::{Path, PathBuf},
};

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> std::io::Result<PathBuf> {
        if path.exists() {
            path.canonicalize()
        } else {
            Ok(self
                .resolve_existing_ancestor(path.parent().ok_or(std::io::ErrorKind::InvalidInput)?)?
                .join(path.file_name().ok_or(std::io::ErrorKind::InvalidInput)?))
        }
    }
}
#[derive(Serialize)]
struct Fact {
    state: &'static str,
}
struct FakeFacade;

struct SubmitFacade {
    fail_first: bool,
    large_wait: bool,
    budgets: Vec<u64>,
    reads: Vec<u64>,
}
impl F3Facade for SubmitFacade {
    type Identity = Value;
    type Submit = Value;
    type Run = Value;
    type Hosts = Value;
    fn identity(
        &mut self,
        _: IdentityInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, FacadeError> {
        Ok(json!({}))
    }
    fn submit(
        &mut self,
        _: SubmitInput,
        budget: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, FacadeError> {
        self.budgets
            .push(budget.remaining(MonoTime::from_micros(0)).as_micros());
        Ok(
            json!({"operation_id":"op", "targets":[{"attempt":"11111111111111111111111111111111"},{"attempt":"22222222222222222222222222222222"}]}),
        )
    }
    fn run(
        &mut self,
        _: RunInput,
        budget: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, FacadeError> {
        self.budgets
            .push(budget.remaining(MonoTime::from_micros(0)).as_micros());
        Ok(
            json!({"timed_out":false, "padding": if self.large_wait { "x".repeat(5000) } else { String::new() }}),
        )
    }
    fn output(
        &mut self,
        input: OutputInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        let OutputInput::Read {
            target, max_bytes, ..
        } = input
        else {
            unreachable!()
        };
        self.reads.push(max_bytes);
        if self.fail_first && self.reads.len() == 1 {
            return Err(FacadeError::NotFound);
        }
        serde_json::from_value(json!({"kind":"chunk", "target":target, "part":"result", "state":"terminal", "reason":null, "offset":0, "next_offset":2, "eof":true, "digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bytes":{"encoding":"utf8", "data":"ok"}})).map_err(|_| FacadeError::StateCorrupt)
    }
    fn hosts(
        &mut self,
        _: HostsInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, FacadeError> {
        Ok(json!({}))
    }
}

#[test]
fn submit_wait_merges_outputs_and_uses_independent_budgets() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = TestClock::new(
        WallTime::from_micros(2_000_000_000_000_000)?,
        MonoTime::from_micros(0),
    );
    let coordination = CoordinationService::open(
        &root,
        ProjectId::from_bytes([1; 16]),
        ActorId::from_bytes([2; 16]),
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let facade = SubmitFacade {
        fail_first: true,
        large_wait: false,
        budgets: vec![],
        reads: vec![],
    };
    let mut service = Service::new(coordination, facade, &clock);
    let response = ToolCall::parse("fabric_run", json!({"op":"submit","wait_seconds":1,"tasks":[{"prompt":"work","route":{"kind":"named","name":"utility"},"timeout_seconds":1}]}))?.execute(&mut service, &CancelToken::new())?.structured_content;
    assert_eq!(
        service.facade.budgets,
        vec![crate::MAX_WAIT_SECONDS * 1_000_000, 1_000_000]
    );
    assert_eq!(response["receipt"]["operation_id"], "op");
    assert_eq!(response["wait"]["timed_out"], false);
    assert_eq!(response["outputs"].as_array().unwrap().len(), 2);
    assert_eq!(response["outputs"][0]["error"]["code"], "not_found");
    assert_eq!(response["outputs"][1]["bytes"]["data"], "ok", "{response}");
    let limits = provenant_fabric_runtime::config::Limits {
        body_bytes: 65536,
        input_frame_bytes: 65536,
        json_depth: 64,
        page_rows: 20,
        output_lines: 100,
        output_bytes: 16384,
        result_bytes: 3000,
        lease_seconds: 300,
        peer_seconds: 30,
        graph_timeout_seconds: 14400,
        store_busy_timeout_ms: 30_000,
        bundle_bytes: 65536,
    };
    service = service.with_limits(limits);
    service.facade.fail_first = false;
    service.facade.reads.clear();
    let request = || {
        ToolCall::parse(
            "fabric_run",
            json!({"op":"submit","wait_seconds":1,"tasks":[{"prompt":"work","route":{"kind":"named","name":"utility"},"timeout_seconds":1}]}),
        )
    };
    let response = request()?
        .execute(&mut service, &CancelToken::new())?
        .structured_content;
    assert_eq!(response["outputs"].as_array().unwrap().len(), 2);
    assert!(
        service
            .facade
            .reads
            .iter()
            .all(|bytes| *bytes > 0 && *bytes < 1500)
    );
    assert!(serde_json::to_vec(&response)?.len() <= 3000);
    service.facade.large_wait = true;
    let response = request()?
        .execute(&mut service, &CancelToken::new())?
        .structured_content;
    assert_eq!(response["receipt"]["operation_id"], "op");
    assert!(response.get("wait").is_none());
    assert!(response.get("outputs").is_none());
    assert!(response["continuation"].as_str().is_some());
    Ok(())
}
impl F3Facade for FakeFacade {
    type Identity = Fact;
    type Submit = Fact;
    type Run = Fact;
    type Hosts = Fact;
    fn identity(
        &mut self,
        _: IdentityInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "identity" })
    }
    fn submit(
        &mut self,
        _: SubmitInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "accepted" })
    }
    fn run(&mut self, _: RunInput, _: WaitBudget, _: &CancelToken) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "unknown" })
    }
    fn output(
        &mut self,
        _: OutputInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        Err(FacadeError::NotFound)
    }
    fn hosts(
        &mut self,
        _: HostsInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "ready" })
    }
}
fn call(
    service: &mut Service<'_, FakeFacade>,
    name: &str,
    input: Value,
) -> Result<Value, Box<dyn Error>> {
    let result = ToolCall::parse(name, input)?.execute(service, &CancelToken::new())?;
    Ok(result.structured_content)
}
#[test]
fn lane_graph_status_reaches_the_owner() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = TestClock::new(
        WallTime::from_micros(2_000_000_000_000_000)?,
        MonoTime::from_micros(0),
    );
    let coordination = CoordinationService::open(
        &root,
        ProjectId::from_bytes([1; 16]),
        ActorId::from_bytes([2; 16]),
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let mut service = Service::new(coordination, FakeFacade, &clock).with_lane_restrictions(true);
    for args in [
        json!({"op":"summary_update","session":"chair","patch":{"now":"work"},"expect_revision":0}),
        json!({"op":"log_append","session":"chair","entry":{"kind":"progress","markdown":"work","channel":"chat"}}),
    ] {
        let result =
            ToolCall::parse("fabric_chair", args)?.execute(&mut service, &CancelToken::new());
        assert_eq!(
            result.err().ok_or("lane mutation accepted")?.code(),
            "lane_coordination_refused"
        );
    }
    let result = ToolCall::Graph(GraphInput::Status {
        extra: Default::default(),
        id: "g-test".into(),
    })
    .execute(&mut service, &CancelToken::new());
    // FakeFacade's default graph owner is unavailable, proving status got
    // through the lane guard without needing a graph run in the fixture.
    assert!(matches!(
        result,
        Err(ServiceError::Facade(FacadeError::OwnerUnavailable))
    ));
    Ok(())
}
#[test]
fn five_tools_reach_the_real_store_through_runtime_and_keep_fences_scoped()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = TestClock::new(
        WallTime::from_micros(2_000_000_000_000_000)?,
        MonoTime::from_micros(0),
    );
    let actor = ActorId::from_bytes([2; 16]);
    let coordination = CoordinationService::open(
        &root,
        ProjectId::from_bytes([1; 16]),
        actor,
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let mut service = Service::new(coordination, FakeFacade, &clock);
    let sent = call(
        &mut service,
        "fabric_mail",
        json!({"op":"send","recipients":[actor],"kind":"note","body":"hello","artifacts":[]}),
    )?;
    let inbox = call(
        &mut service,
        "fabric_mail",
        json!({"op":"read","after":0,"limit":10}),
    )?;
    assert_eq!(inbox["page"]["items"][0]["message"]["id"], sent["id"]);
    let task = call(
        &mut service,
        "fabric_task",
        json!({"op":"create","objective":"Review"}),
    )?;
    let claimed = call(
        &mut service,
        "fabric_task",
        json!({"op":"claim","id":task["task"]["id"],"lease_seconds":1}),
    )?;
    let finished = call(
        &mut service,
        "fabric_task",
        json!({"op":"finish","proof":claimed["proof"],"state":"done"}),
    )?;
    assert_eq!(finished["task"]["state"], "done");
    let noted = call(
        &mut service,
        "fabric_events",
        json!({"op":"note","note":"reviewed"}),
    )?;
    assert!(noted["seq"].as_i64().is_some_and(|seq| seq > 0));
    let work = call(
        &mut service,
        "fabric_lease",
        json!({"op":"claim_acquire","scope":{"kind":"issue","key":"F8a"},"lease_seconds":1}),
    )?;
    let verified = call(
        &mut service,
        "fabric_lease",
        json!({"op":"claim_verify","proof":work["proof"]}),
    )?;
    assert_eq!(verified["claim"]["holder"], json!(actor));
    call(
        &mut service,
        "fabric_lease",
        json!({"op":"claim_release","proof":work["proof"]}),
    )?;
    let oid = "a".repeat(40);
    let landing = call(
        &mut service,
        "fabric_lease",
        json!({"op":"landing_acquire","reference":"refs/heads/main","expected":oid,"candidate":"b".repeat(40),"lease_seconds":1}),
    )?;
    let verified = call(
        &mut service,
        "fabric_lease",
        json!({"op":"landing_verify","proof":landing["proof"]}),
    )?;
    assert_eq!(verified["landing"]["holder"], json!(actor));
    call(
        &mut service,
        "fabric_lease",
        json!({"op":"landing_release","proof":landing["proof"]}),
    )?;
    // F3 is explicitly injected, with no production placeholder.
    assert_eq!(
        call(&mut service, "fabric_context", json!({"op":"inspect"}))?["state"],
        "identity"
    );
    Ok(())
}

use crate::test_facade::local_inbox_owner;
local_inbox_owner!(SubmitFacade);
local_inbox_owner!(FakeFacade);

#[test]
#[allow(clippy::disallowed_methods)] // Native socket framing and read-only WAL deadlines.
fn lane_relay_binds_sender_replays_receipts_and_refreshes_read_only_wal()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = relay_test_clock();
    let project = ProjectId::from_bytes([1; 16]);
    let actor = ActorId::from_bytes([2; 16]);
    let budget = WaitBudget::mcp(clock.mono_now(), 5)?;
    // The host creates the WAL; the lane keeps only a read-only connection.
    let _host_connection = CoordinationService::open(
        &root,
        project,
        actor,
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let host_root = root.clone();
    let handler = move |call: ToolCall, cancel: &CancelToken, within: std::time::Duration| {
        let clock = relay_test_clock();
        let budget = crate::lane::host_budget(&clock, within)?;
        let coordination =
            CoordinationService::open(&host_root, project, actor, &clock, &ThreadPause, cancel)?;
        call.execute_lane_coordination(&coordination, &relay_limits(), budget, cancel)
    };
    let relay_call = |tool: &str, args: Value| -> Result<Value, ServiceError> {
        let request = crate::lane::Request {
            tool: tool.into(),
            arguments: args,
        };
        let (mut client, mut server) =
            std::os::unix::net::UnixStream::pair().map_err(|_| FacadeError::OwnerUnavailable)?;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                crate::lane::serve_one(
                    &mut server,
                    provenant_records::Limits {
                        max_bytes: 65536,
                        max_depth: 64,
                        max_members: 100_000,
                    },
                    &handler,
                    &CancelToken::new(),
                )
            });
            crate::lane::exchange(&mut client, &request, &CancelToken::new())
        })
    };
    let store = provenant_fabric_store::Store::open_read_only(
        &root,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )?
    .ok_or("missing store")?
    .live_read_only()?;
    // A forged lane actor cannot influence the host's captured actor.
    let coordination = CoordinationService::new(
        store,
        project,
        ActorId::from_bytes([99; 16]),
        &clock,
        &ThreadPause,
    );
    let mut lane = Service::new(coordination, FakeFacade, &clock).with_lane_restrictions(true);
    let args = json!({"op":"send","recipients":[actor],"kind":"note","body":"hello host","artifacts":[],"operation_id":"relay-send"});
    let sent = relay_call("fabric_mail", args.clone())?;
    assert_eq!(sent, relay_call("fabric_mail", args)?);
    let task = relay_call(
        "fabric_task",
        json!({"op":"create","objective":"Review","operation_id":"relay-task"}),
    )?;
    let claimed = relay_call(
        "fabric_task",
        json!({"op":"claim","id":task["task"]["id"],"lease_seconds":10,"operation_id":"relay-claim"}),
    )?;
    assert_eq!(claimed["result"], "claimed");
    // A long-lived reader observes changes made after its first observation.
    let before = call(&mut lane, "fabric_task", json!({"op":"list","limit":10}))?;
    assert_eq!(before["page"]["items"][0]["owner"], json!(actor));
    relay_call(
        "fabric_task",
        json!({"op":"finish","proof":claimed["proof"],"state":"done","operation_id":"relay-finish"}),
    )?;
    let after = call(&mut lane, "fabric_task", json!({"op":"list","limit":10}))?;
    assert_ne!(before, after);
    let host = CoordinationService::open(
        &root,
        project,
        actor,
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let mail = serde_json::to_value(host.mail(
        MailInput::Read {
            extra: Default::default(),
            after: Some(0),
            limit: 10,
        },
        budget,
        &CancelToken::new(),
    )?)?;
    let messages = mail["page"]["items"].as_array().ok_or("mail")?;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["message"]["sender"], json!(actor));
    assert_eq!(messages[0]["message"]["id"], sent["id"]);
    let held = serde_json::to_value(host.mutate(
        Mutation::Landing(LandingInput::Acquire {
            extra: Default::default(),
            reference: "refs/heads/main".into(),
            expected: provenant_core::GitOid::parse(&"a".repeat(40))?,
            candidate: provenant_core::GitOid::parse(&"b".repeat(40))?,
            lease_seconds: 10,
        }),
        &provenant_core::OperationId::parse("host-landing")?,
        budget,
        &CancelToken::new(),
    )?)?;
    let verified = relay_call(
        "fabric_lease",
        json!({"op":"landing_verify","proof":held["proof"]}),
    )?;
    assert_eq!(verified["result"], "verified");
    let relayed_read = relay_call("fabric_mail", json!({"op":"read","after":0,"limit":10}))?;
    assert_eq!(
        relayed_read["page"]["items"][0]["message"]["sender"],
        json!(actor)
    );
    let forged = crate::lane::Request {
        tool: "fabric_mail".into(),
        arguments: json!({"op":"send","sender":actor,"recipients":[actor],"kind":"note","body":"forged"}),
    };
    assert_eq!(
        relay_call(&forged.tool, forged.arguments)
            .err()
            .ok_or("forgery accepted")?
            .code(),
        "invalid_input"
    );
    let landing = crate::lane::Request {
        tool: "fabric_lease".into(),
        arguments: json!({"op":"landing_acquire","reference":"refs/heads/main","expected":"a".repeat(40),"candidate":"b".repeat(40),"lease_seconds":1}),
    };
    assert_eq!(
        relay_call(&landing.tool, landing.arguments)
            .err()
            .ok_or("landing accepted")?
            .code(),
        "lane_coordination_refused"
    );
    assert!(
        call(
            &mut lane,
            "fabric_events",
            json!({"op":"note","note":"closed"})
        )
        .is_err()
    );
    Ok(())
}

fn relay_test_clock() -> TestClock {
    TestClock::new(
        WallTime::from_micros(2_000_000_000_000_000).unwrap(),
        MonoTime::from_micros(0),
    )
}

#[test]
#[allow(clippy::disallowed_methods)] // Exercise real host wait and response deadlines.
fn empty_one_second_host_wait_returns_timeout_data_before_transport_expiry()
-> Result<(), Box<dyn Error>> {
    struct Realtime(std::time::Instant);
    impl Clock for Realtime {
        fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
            WallTime::from_micros(2_000_000_000_000_000)
        }
        fn mono_now(&self) -> MonoTime {
            MonoTime::from_micros(self.0.elapsed().as_micros() as u64)
        }
        fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
            while self.mono_now() < deadline {
                std::thread::park_timeout(std::time::Duration::from_micros(
                    deadline
                        .as_micros()
                        .saturating_sub(self.mono_now().as_micros()),
                ));
            }
            Ok(())
        }
    }
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = Realtime(std::time::Instant::now());
    let cancel = CancelToken::new();
    let project = ProjectId::from_bytes([1; 16]);
    let actor = ActorId::from_bytes([2; 16]);
    let _host = CoordinationService::open(&root, project, actor, &clock, &ThreadPause, &cancel)?;
    let (mut client, mut server) = std::os::unix::net::UnixStream::pair()?;
    let request = ToolCall::parse(
        "fabric_events",
        json!({"op":"read","after":0,"limit":10,"wait_seconds":1}),
    )?
    .lane_request()?;
    std::thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let serving = scope.spawn(|| {
            crate::lane::serve_one(
                &mut server,
                provenant_records::Limits::CONFIG,
                &|call, cancel, within| {
                    let budget = crate::lane::host_budget(&clock, within)?;
                    let owner = CoordinationService::open(
                        &root,
                        project,
                        actor,
                        &clock,
                        &ThreadPause,
                        cancel,
                    )?;
                    call.execute_lane_coordination(&owner, &relay_limits(), budget, cancel)
                },
                &cancel,
            )
        });
        let result = crate::lane::exchange_within(
            &mut client,
            &request,
            &cancel,
            std::time::Duration::from_secs(1),
        )?;
        assert_eq!(result["timed_out"], true);
        serving.join().unwrap()?;
        Ok(())
    })
}

fn relay_limits() -> provenant_fabric_runtime::config::Limits {
    provenant_fabric_runtime::config::Limits {
        body_bytes: 65536,
        input_frame_bytes: 65536,
        json_depth: 64,
        page_rows: 20,
        output_lines: 100,
        output_bytes: 16384,
        result_bytes: 3000,
        lease_seconds: 300,
        peer_seconds: 30,
        graph_timeout_seconds: 14400,
        bundle_bytes: 65536,
        store_busy_timeout_ms: 30_000,
    }
}
