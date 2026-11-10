//! The MCP dependency edge is runtime-only, including the real-store tests.
use crate::tools::*;
use provenant_core::{
    ActorId, CancelToken, Clock, DurationMicros, MonoTime, PathResolver, ProjectId, RootInputs,
    TestClock, WaitBudget, WallTime,
};
use provenant_fabric_runtime::coordination::{CoordinationService, ThreadPause};
use provenant_fabric_runtime::inbox::{InboxError, InboxInput, InboxResult};
use provenant_records::control::{
    ControlKind, ControlReceipt, ControlState, ControlSupport, LaunchKind,
};
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

/// A clock the fake facade and the service share, so a simulated wait can
/// exhaust its budget before the facade returns `Timeout`.
#[derive(Clone)]
struct SharedClock(std::sync::Arc<std::sync::Mutex<TestClock>>);
impl SharedClock {
    fn new(wall: WallTime, mono: MonoTime) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(TestClock::new(
            wall, mono,
        ))))
    }
    fn advance(&self, duration: DurationMicros) {
        let mut clock = self.0.lock().unwrap();
        clock.advance(duration).unwrap();
    }
}
impl Clock for SharedClock {
    fn wall_now(&self) -> Result<WallTime, provenant_core::TimeError> {
        self.0.lock().unwrap().wall_now()
    }
    fn mono_now(&self) -> MonoTime {
        self.0.lock().unwrap().mono_now()
    }
    fn wait_until(&self, deadline: MonoTime) -> Result<(), provenant_core::TimeError> {
        self.0.lock().unwrap().wait_until(deadline)
    }
    fn host_monotonic_micros(&self) -> Result<u64, provenant_core::TimeError> {
        self.0.lock().unwrap().host_monotonic_micros()
    }
    fn boot_identity(&self) -> Result<String, provenant_core::TimeError> {
        self.0.lock().unwrap().boot_identity()
    }
}

struct FakeFacade;

struct WaitFacade {
    fail_first: bool,
    large_wait: bool,
    expire_run: bool,
    run_unsatisfied: bool,
    run_io: bool,
    clock: Option<SharedClock>,
    budgets: Vec<u64>,
    reads: Vec<u64>,
}
impl F3Facade for WaitFacade {
    type Identity = Value;
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
    fn run(
        &mut self,
        _: RunInput,
        budget: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, FacadeError> {
        self.budgets
            .push(budget.remaining(MonoTime::from_micros(0)).as_micros());
        if self.run_io {
            return Err(FacadeError::Io {
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
                cause: None,
                operation: "read".into(),
                path: Some("/private/host/wait".into()),
            });
        }
        if self.expire_run {
            if let Some(clock) = &self.clock {
                clock.advance(DurationMicros::from_seconds(2 * crate::MAX_WAIT_SECONDS).unwrap());
            }
            return Err(FacadeError::Timeout);
        }
        if self.run_unsatisfied {
            Ok(
                json!({"timed_out":false,"satisfied":false,"reason":"provider silent; lane attempt-1 is still running"}),
            )
        } else {
            Ok(
                json!({"timed_out":false, "padding": if self.large_wait { "x".repeat(5000) } else { String::new() }}),
            )
        }
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
            return Err(FacadeError::Io {
                delivery_limit: None,
                delivery_stage: None,
                resource: None,
                input_failure: None,
                cause: None,
                operation: "read".into(),
                path: Some("/private/host/output".into()),
            });
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
fn wait_that_expires_before_observation_reports_timed_out() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
    inputs.state_dir = Some(base.join("state"));
    let root = inputs.resolve(&Resolver)?.state;
    let clock = SharedClock::new(
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
    let facade = WaitFacade {
        fail_first: false,
        large_wait: false,
        expire_run: true,
        run_unsatisfied: false,
        run_io: false,
        clock: Some(clock.clone()),
        budgets: vec![],
        reads: vec![],
    };
    let mut service = Service::new(coordination, facade, &clock);
    let response = ToolCall::parse("fabric_run", json!({"op":"wait","targets":[{"attempt":"11111111111111111111111111111111"}],"condition":"all","wait_seconds":1}))?
        .execute(&mut service, &CancelToken::new())?
        .structured_content;
    assert_eq!(response["timed_out"], true, "{response}");
    assert_eq!(response["satisfied"], false, "{response}");
    assert_eq!(
        response["observation_error"]["code"], "timeout",
        "{response}"
    );
    Ok(())
}

impl F3Facade for FakeFacade {
    type Identity = Fact;
    type Run = Fact;
    type Hosts = Fact;
    // An existing Codex attempt has no live path, so steering gets an ordinary receipt.
    fn inbox(
        &mut self,
        input: InboxInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<InboxResult, InboxError> {
        let InboxInput::Steer {
            attempt,
            operation_id,
            ..
        } = input
        else {
            return Err(InboxError::OwnerUnavailable);
        };
        Ok(InboxResult::Control {
            receipt: ControlReceipt {
                extra: Default::default(),
                operation_id: operation_id.ok_or(InboxError::Invalid("operation_id"))?,
                control: ControlKind::Steer,
                target: attempt.to_string(),
                launch: LaunchKind::Lane,
                support: ControlSupport::ContinuationOnly,
                transport: None,
                state: ControlState::Unsupported,
                reason: ControlSupport::ContinuationOnly.reason().into(),
                message_id: None,
            },
        })
    }
    fn identity(
        &mut self,
        _: IdentityInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "identity" })
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
fn graph_documents_reach_the_shared_owner_unchanged() -> Result<(), Box<dyn Error>> {
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
    let trace = ToolCall::parse("fabric_output", json!({"op":"trace","id":"g-test"}))?;
    assert_eq!(trace.name(), "fabric_output");
    assert_eq!(
        trace
            .execute(&mut service, &CancelToken::new())?
            .structured_content,
        json!({"op":"output","id":"g-test","trace":true})
    );
    let status = json!({"op":"status","id":"g-test","wait_seconds":1});
    assert_eq!(call(&mut service, "fabric_run", status.clone())?, status);
    let child_request = json!({"op":"run","brief":"inspect parser","use":"explore","tier":"junior","wait_seconds":1});
    let child = call(&mut service, "fabric_run", child_request)?;
    assert_eq!(child["receipt"]["op"], "add");
    assert_eq!(child["receipt"]["use"], "explore");
    assert_eq!(child["receipt"]["brief"], "inspect parser");
    assert_eq!(child["receipt"]["tier"], "junior");
    service = service.with_lane_restrictions(false);
    for request in [
        json!({"op":"run","document":{"id":"graph","nodes":[{"id":"inspect","use":"task","brief":"inspect parser"}],"templates":{"local":{"brief":"local"}}}}),
        json!({"op":"edit","id":"graph","patch":{"expected_revision":1,"idempotency_key":"edit-1","nodes":[{"id":"next","use":"local"}]}}),
        json!({"op":"stop","id":"graph","reason":"maintenance","wait_seconds":1}),
        json!({"op":"retry","id":"graph","node":"task","brief":"repeat","extend":["attempts=2"],"wait_seconds":1}),
    ] {
        assert_eq!(call(&mut service, "fabric_run", request.clone())?, request);
    }
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
local_inbox_owner!(WaitFacade);
impl McpFacade for FakeFacade {
    fn graph_document(
        &mut self,
        input: Value,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Value, ServiceError> {
        Ok(input)
    }
    fn inbox_owner(
        &mut self,
        input: InboxInput,
        host: Option<String>,
        budget: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<InboxResult, ServiceError> {
        if host.is_some() {
            return Err(FacadeError::InvalidInput { detail: None }.into());
        }
        Ok(self.inbox(input, budget, cancel)?)
    }

    fn add_from_lane(
        &mut self,
        operation: &provenant_fabric_runtime::graph::relay::RelayOp,
        _: &CancelToken,
        _: std::time::Duration,
    ) -> Result<Value, ServiceError> {
        Ok(serde_json::to_value(operation)?)
    }

    fn wait_for_children(
        &mut self,
        _: &CancelToken,
        within: std::time::Duration,
    ) -> Result<Value, ServiceError> {
        // One second completes; two seconds scripts an observation timeout.
        if within == std::time::Duration::from_secs(2) {
            return Err(ServiceError::Facade(FacadeError::Timeout));
        }
        assert_eq!(within, std::time::Duration::from_secs(1));
        Ok(json!({"children":[{"node":"parent--child", "settled":true}]}))
    }
}

#[test]
fn lane_run_submission_adds_graph_children_instead_of_attempts() -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let mut roots = RootInputs::new(
        temporary.path().join("home"),
        temporary.path().join("checkout"),
    );
    roots.state_dir = Some(temporary.path().join("state"));
    let root = roots.resolve(&Resolver)?.state;
    let clock = TestClock::new(WallTime::from_micros(0)?, MonoTime::from_micros(0));
    let coordination = CoordinationService::open(
        &root,
        ProjectId::from_bytes([1; 16]),
        ActorId::from_bytes([2; 16]),
        &clock,
        &ThreadPause,
        &CancelToken::new(),
    )?;
    let mut service = Service::new(coordination, FakeFacade, &clock).with_lane_restrictions(true);
    let input = json!({
        "op":"run", "idempotency_key":"child-run", "wait_seconds":1, "brief":"implement the child", "tier":"senior",
        "document":{"id":"task", "use":"task", "access":"worktree_write"}
    });
    let invoke = |service: &mut Service<'_, FakeFacade>| -> Result<Value, Box<dyn Error>> {
        Ok(ToolCall::parse("fabric_run", input.clone())?
            .execute(service, &CancelToken::new())?
            .structured_content)
    };
    let first = invoke(&mut service)?;
    let replay = invoke(&mut service)?;
    assert_eq!(first, replay);
    assert_eq!(first["children"]["children"][0]["settled"], true);
    let mut timed_out = input.clone();
    timed_out["wait_seconds"] = json!(2);
    let timed_out = ToolCall::parse("fabric_run", timed_out)?
        .execute(&mut service, &CancelToken::new())?
        .structured_content;
    assert_eq!(
        timed_out["receipt"]["idempotency_key"],
        first["receipt"]["idempotency_key"]
    );
    assert!(timed_out.get("observation_error").is_some());
    let mut no_wait = input.clone();
    no_wait
        .as_object_mut()
        .ok_or("submission object")?
        .remove("wait_seconds");
    let no_wait = ToolCall::parse("fabric_run", no_wait)?
        .execute(&mut service, &CancelToken::new())?
        .structured_content;
    assert_eq!(
        no_wait["idempotency_key"],
        first["receipt"]["idempotency_key"]
    );
    assert!(no_wait.get("wait").is_none());
    assert!(no_wait.get("children").is_none());
    assert!(no_wait.get("observation_error").is_none());
    assert_eq!(first["receipt"]["op"], "add");
    assert_eq!(first["receipt"]["use"], "task");
    assert_eq!(first["receipt"]["tier"], "senior");
    assert_eq!(first["receipt"]["brief"], "implement the child");
    assert_eq!(first["receipt"]["writer"], true);
    assert!(
        !first["receipt"]["idempotency_key"]
            .as_str()
            .ok_or("missing key")?
            .is_empty()
    );
    assert!(first["receipt"].get("targets").is_none());
    for field in ["worktree", "branch", "base", "nodes", "templates"] {
        let mut unsupported = input.clone();
        unsupported["document"][field] = json!("unsupported");
        let error = ToolCall::parse("fabric_run", unsupported)?
            .execute(&mut service, &CancelToken::new())
            .err()
            .ok_or("unsupported launch accepted")?;
        assert_eq!(error.code(), "invalid_input");
    }
    Ok(())
}

#[test]
fn unsupported_steer_is_an_mcp_refusal_with_the_stable_code() -> Result<(), Box<dyn Error>> {
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
    let mut service = Service::new(coordination, FakeFacade, &clock);
    let steer = json!({"op":"steer","attempt":"00000000000000000000000000000001",
        "text":"slow down","operation_id":"steer-1"});
    let Err(error) =
        ToolCall::parse("fabric_inbox", steer)?.execute(&mut service, &CancelToken::new())
    else {
        return Err("unsupported steer returned a success response".into());
    };
    assert_eq!(error.code(), "control_unsupported");
    let detail = error.structured_error();
    assert_eq!(detail["code"], "control_unsupported");
    assert_eq!(detail["state"], "unsupported");
    assert_eq!(detail["operation_id"], "steer-1");
    Ok(())
}

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
    relay_call(
        "fabric_events",
        json!({"op":"note","note":"first","operation_id":"relay-note"}),
    )?;
    // A long-lived reader observes changes made after its first observation.
    let before = call(&mut lane, "fabric_events", json!({"op":"read","limit":10}))?;
    let items = before["page"]["items"].as_array().ok_or("events")?;
    assert_eq!(items.last().ok_or("note")?["record"]["actor"], json!(actor));
    relay_call(
        "fabric_events",
        json!({"op":"note","note":"second","operation_id":"relay-note-2"}),
    )?;
    let after = call(&mut lane, "fabric_events", json!({"op":"read","limit":10}))?;
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
            session: None,
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
        store_busy_timeout_ms: 30_000,
    }
}
