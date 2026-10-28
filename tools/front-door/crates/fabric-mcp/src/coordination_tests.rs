//! The MCP dependency edge is runtime-only, including the real-store tests.
use crate::tools::*;
use provenant_core::{
    ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
    WaitBudget, WallTime,
};
use provenant_fabric_runtime::coordination::{CoordinationService, ThreadWait};
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
        &ThreadWait,
        WaitBudget::mcp(clock.mono_now(), 5)?,
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
        &ThreadWait,
        WaitBudget::mcp(clock.mono_now(), 5)?,
        &CancelToken::new(),
    )?;
    let mut service = Service::new(coordination, FakeFacade, &clock).with_lane_restrictions(true);
    let result = ToolCall::Graph(GraphInput::Status {
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
        &ThreadWait,
        WaitBudget::mcp(clock.mono_now(), 5)?,
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

impl crate::tools::McpFacade for SubmitFacade {}
impl crate::tools::McpFacade for FakeFacade {}
