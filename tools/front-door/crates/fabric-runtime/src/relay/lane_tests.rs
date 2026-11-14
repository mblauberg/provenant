//! The relay over a real store: the host binds the sender, a retried call
//! returns its stored receipt, and a read-only lane reader sees later writes.
use super::{ServiceError, ToolCall, host_budget};
use crate::config::Limits;
use crate::coordination::{
    ClaimInput, CoordinationApi, CoordinationService, EventsInput, MailInput, Mutation, ThreadPause,
};
use provenant_core::{
    ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
    WaitBudget, WallTime,
};
use serde_json::{Value, json};
use std::{
    error::Error,
    path::{Path, PathBuf},
    time::{Duration, Instant},
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

fn clock() -> TestClock {
    TestClock::new(
        WallTime::from_micros(2_000_000_000_000_000).unwrap(),
        MonoTime::from_micros(0),
    )
}

fn limits() -> Limits {
    Limits {
        body_bytes: 65536,
        input_frame_bytes: 65536,
        json_depth: 64,
        page_rows: 20,
        output_lines: 100,
        output_bytes: 16384,
        result_bytes: 3000,
        peer_seconds: 30,
        graph_timeout_seconds: 14400,
        store_busy_timeout_ms: 30_000,
    }
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
    let clock = clock();
    let project = ProjectId::from_bytes([1; 16]);
    let actor = ActorId::from_bytes([2; 16]);
    let budget = WaitBudget::relay(clock.mono_now(), 5)?;
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
    let handler = move |call: ToolCall, cancel: &CancelToken, within: Duration| {
        let clock = self::clock();
        let budget = host_budget(&clock, within)?;
        let coordination =
            CoordinationService::open(&host_root, project, actor, &clock, &ThreadPause, cancel)?;
        call.execute_lane_coordination(&coordination, &limits(), budget, cancel)
    };
    let relay_call = |tool: &str, args: Value| -> Result<Value, ServiceError> {
        let request = super::host::Request {
            tool: tool.into(),
            arguments: args,
        };
        let (mut client, mut server) = std::os::unix::net::UnixStream::pair()
            .map_err(|_| crate::coordination::facade::FacadeError::OwnerUnavailable)?;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                super::host::serve_one(
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
            super::host::exchange(&mut client, &request, &CancelToken::new())
        })
    };
    let store = provenant_fabric_store::Store::open_read_only(
        &root,
        Instant::now() + Duration::from_secs(5),
    )?
    .ok_or("missing store")?
    .live_read_only()?;
    // A forged lane actor cannot influence the host's captured actor.
    let lane = CoordinationService::new(
        store,
        project,
        ActorId::from_bytes([99; 16]),
        &clock,
        &ThreadPause,
    );
    let read_events = || -> Result<Value, Box<dyn Error>> {
        let input: EventsInput = serde_json::from_value(json!({"op":"read","limit":10}))?;
        Ok(serde_json::to_value(lane.events(
            input,
            budget,
            &CancelToken::new(),
        )?)?)
    };
    let args = json!({"op":"send","recipients":[actor],"kind":"note","body":"hello host","artifacts":[],"operation_id":"relay-send"});
    let sent = relay_call("fabric_mail", args.clone())?;
    assert_eq!(sent, relay_call("fabric_mail", args)?);
    relay_call(
        "fabric_events",
        json!({"op":"note","note":"first","operation_id":"relay-note"}),
    )?;
    // A long-lived reader observes changes made after its first observation.
    let before = read_events()?;
    let items = before["page"]["items"].as_array().ok_or("events")?;
    assert_eq!(items.last().ok_or("note")?["record"]["actor"], json!(actor));
    relay_call(
        "fabric_events",
        json!({"op":"note","note":"second","operation_id":"relay-note-2"}),
    )?;
    assert_ne!(before, read_events()?);
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
        Mutation::Claim(ClaimInput::Acquire {
            extra: Default::default(),
            subject: provenant_records::custody::Subject::new(
                provenant_records::custody::SubjectKind::Issue,
                "host-claim",
            ),
            holder: provenant_records::custody::Holder::Chair {
                chair: provenant_records::custody::Chair {
                    extra: Default::default(),
                    host: provenant_core::HostId::from_bytes([1; 16]),
                    actor,
                    session: None,
                    process: None,
                },
            },
        }),
        &provenant_core::OperationId::parse("host-claim")?,
        budget,
        &CancelToken::new(),
    )?)?;
    let verified = relay_call(
        "fabric_custody",
        json!({"op":"claim_verify","subject":held["change"]["record"]["subject"]}),
    )?;
    assert_eq!(verified["result"], "verified");
    let relayed_read = relay_call("fabric_mail", json!({"op":"read","after":0,"limit":10}))?;
    assert_eq!(
        relayed_read["page"]["items"][0]["message"]["sender"],
        json!(actor)
    );
    // The sender is the host's, never the request's.
    let forged = relay_call(
        "fabric_mail",
        json!({"op":"send","sender":actor,"recipients":[actor],"kind":"note","body":"forged"}),
    );
    assert_eq!(
        forged.err().ok_or("forgery accepted")?.code(),
        "invalid_input"
    );
    // The relay carries coordination only; any other tool is refused.
    assert!(relay_call("fabric_run", json!({"op":"list"})).is_err());
    Ok(())
}
