#![allow(clippy::panic, clippy::unwrap_used)]

use super::*;
use crate::tools::*;
use provenant_core::{
    ActorId, CancelToken, Clock, MonoTime, PathResolver, ProjectId, RootInputs, TestClock,
    WaitBudget, WallTime,
};
use provenant_fabric_runtime::coordination::{CoordinationService, ThreadWait};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const TEST_WAIT: Duration = Duration::from_secs(3);

#[test]
fn framer_preserves_utf8_across_chunks_and_frames() {
    let mut framer = Framer::default();
    framer.push(br#"{"message":"caf"#).unwrap();
    framer.push("é".as_bytes()).unwrap();
    framer.push(b"\"}\n{}\n").unwrap();
    assert_eq!(
        framer.next_frame().unwrap(),
        Some("{\"message\":\"café\"}".as_bytes().to_vec())
    );
    assert_eq!(framer.next_frame().unwrap(), Some(b"{}".to_vec()));
    assert_eq!(framer.next_frame().unwrap(), None);
    assert!(!framer.incomplete());
}

#[test]
fn framer_enforces_the_frame_limit_and_reports_partial_eof() {
    let mut exact = Framer::default();
    exact.push(&vec![b'x'; MAX_FRAME]).unwrap();
    exact.push(b"\n").unwrap();
    assert_eq!(
        exact.next_frame().unwrap().map(|frame| frame.len()),
        Some(MAX_FRAME)
    );
    let mut oversized = Framer::default();
    oversized.push(&vec![b'x'; MAX_FRAME]).unwrap();
    oversized.push(b"x").unwrap();
    assert!(matches!(oversized.next_frame(), Err(Error::Oversize)));
    let mut partial = Framer::default();
    partial.push(br#"{"jsonrpc":"2.0"}"#).unwrap();
    assert!(partial.incomplete());
    assert_eq!(partial.next_frame().unwrap(), None);
}

#[test]
fn serve_splits_utf8_across_chunks_and_executes_typed_calls() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    let init = initialize();
    let initialized = notification("notifications/initialized", json!({}));
    let prefix_len = encode_line(&init).len() + encode_line(&initialized).len();
    server.send(&init);
    server.send(&initialized);
    let call = tool_call(
        "unicode",
        "fabric_task",
        json!({"op":"create","objective":"café 🦜"}),
    );
    let line = encode_line(&call);
    let split = line
        .windows(2)
        .position(|window| window == [0xc3, 0xa9])
        .unwrap()
        + 1;
    server.send_bytes(&line[..split]);
    assert!(server.wait_for_consumed(prefix_len + split, TEST_WAIT));
    server.send_bytes(&line[split..]);
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["id"], "init");
    assert_eq!(frames[1]["id"], "unicode");
    assert_eq!(
        frames[1]["result"]["structuredContent"]["task"]["objective"],
        "café 🦜"
    );
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
// MCP 2026-07-28 Server > Tools > Error Handling:
// https://modelcontextprotocol.io/specification/2026-07-28/server/tools#error-handling
fn invalid_tool_arguments_are_results_with_the_field_path() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.send(&tool_call(
        "bad",
        "fabric_identity",
        json!({"op":"inspect","extra":true}),
    ));
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["id"], "bad");
    assert!(frames[1]["result"]["isError"].as_bool().unwrap());
    assert!(frames[1].get("error").is_none());
    assert_eq!(
        frames[1]["result"]["structuredContent"]["code"],
        "invalid_input"
    );
    assert!(
        frames[1]["result"]["structuredContent"]["path"]
            .as_str()
            .unwrap()
            .contains("extra")
    );
    assert!(
        frames[1]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("extra")
    );
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn unknown_tool_is_a_protocol_error_and_lifecycle_errors_stay_ordered() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send(&json!({"jsonrpc":"2.0","id":"early","method":"tools/list"}));
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.send(&tool_call("unknown", "fabric_missing", json!({})));
    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["error"]["code"], -32600);
    assert_eq!(frames[0]["id"], "early");
    assert_eq!(frames[1]["id"], "init");
    assert_eq!(frames[2]["error"]["code"], -32602);
    assert_eq!(frames[2]["id"], "unknown");
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn half_closed_stdin_drains_a_call_already_in_flight() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    assert_eq!(
        server.wait_for_frames(1, TEST_WAIT).unwrap()[0]["id"],
        "init"
    );
    server.send(&tool_call(
        "in-flight",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    assert!(
        gate.wait_for_entered(1, TEST_WAIT),
        "frames: {:?}",
        server.captured_frames()
    );
    server.close_input();
    assert!(server.no_more_frames(1, Duration::from_millis(50)));
    gate.release();
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["id"], "in-flight");
    assert!(frames[1]["result"]["isError"].as_bool().unwrap());
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn slow_stdout_reader_is_given_more_than_the_old_quarter_second() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.shared.output.lock().unwrap().delay = Duration::from_millis(350);
    server.send(&initialize());
    assert_eq!(
        server.wait_for_frames(1, TEST_WAIT).unwrap()[0]["id"],
        "init"
    );
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn live_request_ids_are_rejected_until_completion_then_can_be_reused() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.wait_for_frames(1, TEST_WAIT).unwrap();
    server.send(&tool_call(
        "reused",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    assert!(gate.wait_for_entered(1, TEST_WAIT));
    server.send(&json!({"jsonrpc":"2.0","id":"reused","method":"tools/list"}));
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["error"]["code"], -32600);
    gate.release();
    assert_eq!(
        server.wait_for_frames(3, TEST_WAIT).unwrap()[2]["id"],
        "reused"
    );
    server.send(&json!({"jsonrpc":"2.0","id":"reused","method":"tools/list"}));
    let frames = server.wait_for_frames(4, TEST_WAIT).unwrap();
    assert_eq!(frames[3]["id"], "reused");
    assert!(frames[3].get("error").is_none());
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn cancellation_suppresses_late_observations_without_cancelling_runs() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.send(&tool_call(
        "late",
        "fabric_run",
        json!({"op":"status","target":{"attempt":"00000000000000000000000000000001"}}),
    ));
    assert!(gate.wait_for_entered(1, TEST_WAIT));
    server.send(&notification(
        "notifications/cancelled",
        json!({"requestId":"late"}),
    ));
    server.send(&json!({"jsonrpc":"2.0","id":"barrier","method":"tools/list"}));
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["id"], "barrier");
    gate.release();
    assert!(gate.wait_for_returned(1, TEST_WAIT));
    server.send(&json!({"jsonrpc":"2.0","id":"after","method":"tools/list"}));
    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert_eq!(frames[2]["id"], "after");
    assert!(server.no_more_frames(3, Duration::from_millis(100)));
    assert_eq!(gate.run_cancel_calls(), 0);
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn worker_panic_completes_its_request_and_other_workers_remain_available() {
    let gate = Arc::new(Gate::default());
    gate.panic_next(1);
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.send(&tool_call(
        "panic",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["id"], "panic");
    assert_eq!(
        frames[1]["result"]["structuredContent"]["code"],
        "worker_panicked"
    );
    gate.release();
    server.send(&tool_call(
        "after-panic",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert_eq!(frames[2]["id"], "after-panic");
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn all_workers_dead_is_fatal() {
    let gate = Arc::new(Gate::default());
    gate.panic_next(WORKERS);
    let server = Server::start(FakeFactory::new(gate), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    let mut calls = Vec::new();
    for id in 0..WORKERS {
        calls.extend_from_slice(&encode_line(&tool_call(
            &format!("panic-{id}"),
            "fabric_identity",
            json!({"op":"inspect"}),
        )));
    }
    server.send_bytes(&calls);
    let result = server.wait_result(TEST_WAIT);
    assert!(matches!(result, Err(Error::Worker)), "result: {result:?}");
}

#[test]
fn four_workers_and_queue_capacity_return_busy_after_thirty_six_calls() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    assert!(server.wait_for_frames(1, TEST_WAIT).is_some());
    let mut calls = Vec::new();
    for id in 0..WORKERS {
        calls.extend_from_slice(&encode_line(&tool_call(
            &id.to_string(),
            "fabric_identity",
            json!({"op":"inspect"}),
        )));
    }
    server.send_bytes(&calls);
    assert!(gate.wait_for_entered(WORKERS, TEST_WAIT));
    calls.clear();
    for id in WORKERS..WORKERS + QUEUE {
        calls.extend_from_slice(&encode_line(&tool_call(
            &id.to_string(),
            "fabric_identity",
            json!({"op":"inspect"}),
        )));
    }
    calls.extend_from_slice(&encode_line(
        &json!({"jsonrpc":"2.0","id":"queue-barrier","method":"tools/list"}),
    ));
    server.send_bytes(&calls);
    assert_eq!(
        server.wait_for_frames(2, TEST_WAIT).unwrap()[1]["id"],
        "queue-barrier"
    );
    server.send(&tool_call(
        "overflow",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert!(frames[2]["result"]["isError"].as_bool().unwrap());
    assert_eq!(frames[2]["result"]["structuredContent"]["code"], "busy");
    gate.release();
    assert!(gate.wait_for_returned(WORKERS + QUEUE, TEST_WAIT));
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn eof_inside_a_frame_emits_parse_error_then_closes() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send_bytes(br#"{"jsonrpc":"2.0","id":"partial"}"#);
    server.close_input();
    let frames = server.wait_for_frames(1, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["error"]["code"], -32700);
    assert_eq!(frames[0]["error"]["message"], "EOF inside a JSON frame");
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn modern_eof_error_has_no_uncorrelated_id() {
    // MCP 2026-07-28 basic transports: parse errors omit id when unavailable.
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send(&json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":{protocol::VERSION:protocol::MODERN,protocol::CAPABILITIES:{}}}}));
    server.wait_for_frames(1, TEST_WAIT).unwrap();
    server.send_bytes(b"{");
    server.close_input();
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["error"]["code"], -32700);
    assert!(frames[1].get("id").is_none());
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn oversize_frame_emits_a_diagnostic_and_broken_stdout_stops_writes() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send_bytes(&vec![b'x'; MAX_FRAME + 1]);
    let (result, frames) = server.finish_with_frames();
    assert!(matches!(result, Err(Error::Oversize)));
    assert_eq!(frames[0]["error"]["code"], -32700);

    let broken = Server::start(FakeFactory::new(Arc::new(Gate::default())), true);
    broken.send_bytes(b"{\n");
    assert!(
        matches!(broken.finish(), Err(Error::Output(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
}

#[test]
fn oversized_tool_reply_is_replaced_by_an_error_result_before_write() {
    let shared = Arc::new(Shared::default());
    let mut output = PipeOutput(shared.clone());
    let id = Id::Text("too-large".into());
    let payload = json!({"content":[{"type":"text","text":"x".repeat(MAX_FRAME)}],"isError":false});
    let reply = protocol::result(&id, payload, false);
    write_before(&mut output, &reply, Instant::now() + TEST_WAIT).unwrap();
    let frames = shared.output.lock().unwrap();
    let actual: Value = serde_json::from_slice(&frames.frames[0]).unwrap();
    assert!(actual["result"]["isError"].as_bool().unwrap());
    assert_eq!(
        actual["result"]["structuredContent"]["code"],
        "result_too_large"
    );
}

// All production deadlines are retained by TIMEOUTS; exercise shutdown in ms.
const SHORT_TIMEOUTS: Timeouts = Timeouts {
    input_read: Duration::from_millis(40),
    event: Duration::from_millis(2),
    write: Duration::from_millis(30),
    shutdown: Duration::from_millis(80),
    cancel_grace: Duration::from_millis(20),
};

fn completed(id: &Id) -> Event {
    Event::Completed(Completion {
        id: id.clone(),
        reply: protocol::result(id, json!({"content":[],"isError":false}), false),
    })
}

#[test]
fn shutdown_deadline_cancels_a_straggler_and_never_writes_its_late_reply() {
    let gate = Arc::new(Gate::default());
    let server = Server::start_with_timeouts(FakeFactory::new(gate.clone()), false, SHORT_TIMEOUTS);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.wait_for_frames(1, TEST_WAIT).unwrap();
    server.send(&tool_call(
        "straggler",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    assert!(gate.wait_for_entered(1, TEST_WAIT));
    let started = Instant::now();
    server.close_input();
    let result = server.wait_result(TEST_WAIT);
    assert!(matches!(result, Err(Error::Shutdown)), "{result:?}");
    assert!(started.elapsed() < Duration::from_millis(500));
    {
        let state = gate.state.lock().unwrap();
        assert_eq!(state.returned, 0, "straggler must outlive the deadline");
        assert!(state.tokens[0].is_cancelled());
    }
    gate.release();
    assert!(gate.wait_for_returned(1, TEST_WAIT));
    assert_eq!(server.captured_frames().len(), 1, "no late reply");
}

#[test]
fn drain_write_failure_stops_writes_and_cancels_remaining_jobs() {
    struct BrokenDrain(usize);
    impl Output for BrokenDrain {
        fn write_frame(&mut self, _: &[u8], _: Instant) -> io::Result<()> {
            self.0 += 1;
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    let (send, events) = mpsc::sync_channel(EVENT_QUEUE);
    let first = Id::Text("first".into());
    let mut live = BTreeMap::from([(first.clone(), CancelToken::new())]);
    // This is already a completion when draining starts: failure is in the
    // drain's writer, rather than the normal admission loop's writer.
    send.send(completed(&first)).unwrap();
    let mut tokens = Vec::new();
    let mut workers = Vec::new();
    for name in ["remaining-1", "remaining-2"] {
        let id = Id::Text(name.into());
        let cancel = CancelToken::new();
        tokens.push(cancel.clone());
        live.insert(id.clone(), cancel.clone());
        let send = send.clone();
        workers.push(thread::spawn(move || {
            let deadline = Instant::now() + TEST_WAIT;
            while !cancel.is_cancelled() && Instant::now() < deadline {
                thread::park_timeout(Duration::from_millis(1));
            }
            send.send(completed(&id)).unwrap();
        }));
    }
    let mut output = BrokenDrain(0);
    let started = Instant::now();
    let result = shutdown_workers(
        &events,
        &mut output,
        &mut live,
        &mut workers,
        false,
        Timeouts {
            shutdown: Duration::from_millis(400),
            ..SHORT_TIMEOUTS
        },
    );
    assert!(
        matches!(result, Err(Error::Output(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "cancellation must follow the write failure, not the shutdown deadline"
    );
    assert!(tokens.iter().all(CancelToken::is_cancelled));
    assert!(live.is_empty());
    assert!(workers.is_empty());
    assert_eq!(
        output.0, 1,
        "late completions must not attempt another write"
    );
}

#[test]
fn reader_join_finishes_after_a_pending_read_honours_its_deadline() {
    struct DeadlineInput {
        first: Option<Vec<u8>>,
        entered: mpsc::SyncSender<Instant>,
        held: mpsc::Receiver<()>,
    }
    impl Input for DeadlineInput {
        fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
            if let Some(first) = self.first.take() {
                bytes[..first.len()].copy_from_slice(&first);
                return Ok(first.len());
            }
            self.entered.send(deadline).unwrap();
            assert!(matches!(
                self.held
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
            Err(io::ErrorKind::TimedOut.into())
        }
    }
    struct FailWhileReading(mpsc::Receiver<Instant>, Option<Instant>);
    impl Output for FailWhileReading {
        fn write_frame(&mut self, _: &[u8], _: Instant) -> io::Result<()> {
            self.1 = Some(self.0.recv_timeout(TEST_WAIT).unwrap());
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    let (entered, reading) = mpsc::sync_channel(1);
    let (_release, held) = mpsc::sync_channel(1);
    let mut input = DeadlineInput {
        first: Some(encode_line(&initialize())),
        entered,
        held,
    };
    let mut output = FailWhileReading(reading, None);
    let factory = FakeFactory::new(Arc::new(Gate::default()));
    let started = Instant::now();
    let result = serve_with_timeouts(&mut input, &mut output, factory, SHORT_TIMEOUTS);
    assert!(matches!(result, Err(Error::Output(_))), "{result:?}");
    assert!(
        Instant::now() >= output.1.unwrap(),
        "the pending read was joined"
    );
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn maximum_fetch_payloads_fit_the_real_reply_frame_without_truncation() {
    use provenant_fabric_runtime::coordination::input::MAX_FETCH_BYTES;
    // NUL needs six JSON bytes per input byte; 255 needs three digits plus
    // a separator in the invalid-UTF-8 byte-array representation.
    for (byte, encoding) in [(0_u8, "utf8"), (255, "bytes")] {
        for modern in [false, true] {
            let gate = Arc::new(Gate::default());
            gate.state.lock().unwrap().output_bytes =
                vec![byte; usize::try_from(MAX_FETCH_BYTES).unwrap()];
            let server = Server::start(FakeFactory::new(gate), false);
            if modern {
                server.send(&json!({"jsonrpc":"2.0","id":"discover","method":"server/discover","params":{"_meta":{protocol::VERSION:protocol::MODERN,protocol::CAPABILITIES:{}}}}));
            } else {
                server.send(&initialize());
                server.send(&notification("notifications/initialized", json!({})));
            }
            let mut call = tool_call(
                "fetch",
                "fabric_output",
                json!({
                    "op":"fetch", "target":{"attempt":"00000000000000000000000000000001"},
                    "artifact":{"path":{"encoding":"utf8","path":"result.txt"},"digest":null},
                    "max_bytes":MAX_FETCH_BYTES
                }),
            );
            if modern {
                call["params"]["_meta"] =
                    json!({protocol::VERSION:protocol::MODERN,protocol::CAPABILITIES:{}});
            }
            server.send(&call);
            let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
            let result = &frames[1]["result"];
            assert_eq!(frames[1]["id"], "fetch");
            assert_ne!(result["isError"], true, "{result}");
            let bytes = &result["structuredContent"]["bytes"];
            assert_eq!(bytes["encoding"], encoding);
            if byte == 0 {
                assert_eq!(
                    bytes["data"].as_str().unwrap().as_bytes(),
                    vec![byte; usize::try_from(MAX_FETCH_BYTES).unwrap()]
                );
            } else {
                let data = bytes["data"].as_array().unwrap();
                assert_eq!(data.len(), usize::try_from(MAX_FETCH_BYTES).unwrap());
                assert!(data.iter().all(|value| *value == byte));
            }
            let output = server.shared.output.lock().unwrap();
            assert!(output.frames[1].len() <= MAX_FRAME, "including newline");
            drop(output);
            assert!(matches!(server.finish(), Ok(())));
        }
    }
}

#[test]
fn shipped_clients_first_requests_negotiate_the_supported_eras() {
    // Verbatim first stdio frames captured on 24 October 2026.
    for (fixture, modern) in [
        (
            include_bytes!("../tests/fixtures/claude-code-2.1.309-discover.json").as_slice(),
            true,
        ),
        (
            include_bytes!("../tests/fixtures/claude-code-2.1.309-initialize.json").as_slice(),
            false,
        ),
        (
            include_bytes!("../tests/fixtures/codex-0.159.0-initialize.json").as_slice(),
            false,
        ),
    ] {
        let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
        server.send_bytes(fixture);
        let frames = server.wait_for_frames(1, TEST_WAIT).unwrap();
        let request: Value = serde_json::from_slice(fixture).unwrap();
        assert_eq!(frames[0]["id"], request["id"]);
        assert!(frames[0].get("error").is_none());
        let expected = if modern {
            json!({"supportedVersions":["2026-07-28","2025-11-25"],"capabilities":{"tools":{}},"ttlMs":300_000,"cacheScope":"public",
                "resultType":"complete", "_meta":{"io.modelcontextprotocol/serverInfo":{"name":"provenant-fabric","version":env!("CARGO_PKG_VERSION")}}})
        } else {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"provenant-fabric","version":env!("CARGO_PKG_VERSION")}})
        };
        assert_eq!(
            frames[0],
            json!({"jsonrpc":"2.0", "id":request["id"], "result":expected})
        );
        assert!(matches!(server.finish(), Ok(())));
    }
}

#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}
#[derive(Default)]
struct GateState {
    entered: usize,
    returned: usize,
    released: bool,
    panic_remaining: usize,
    run_cancel_calls: usize,
    output_bytes: Vec<u8>,
    tokens: Vec<CancelToken>,
}
impl Gate {
    fn hold(&self, cancel: &CancelToken) {
        let mut state = self.state.lock().unwrap();
        state.entered += 1;
        state.tokens.push(cancel.clone());
        self.changed.notify_all();
        let deadline = Instant::now() + TEST_WAIT;
        while !state.released {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (next, timeout) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            if timeout.timed_out() {
                break;
            }
        }
        state.returned += 1;
        self.changed.notify_all();
    }
    fn release(&self) {
        self.state.lock().unwrap().released = true;
        self.changed.notify_all();
    }
    fn panic_next(&self, count: usize) {
        self.state.lock().unwrap().panic_remaining = count;
    }
    fn maybe_panic(&self) {
        let should_panic = {
            let mut state = self.state.lock().unwrap();
            if state.panic_remaining > 0 {
                state.panic_remaining -= 1;
                true
            } else {
                false
            }
        };
        if should_panic {
            panic!("simulated facade panic");
        }
    }
    fn run_cancel_call(&self) {
        self.state.lock().unwrap().run_cancel_calls += 1;
    }
    fn run_cancel_calls(&self) -> usize {
        self.state.lock().unwrap().run_cancel_calls
    }
    fn wait_for_entered(&self, count: usize, timeout: Duration) -> bool {
        self.wait_for(|state| state.entered >= count, timeout)
    }
    fn wait_for_returned(&self, count: usize, timeout: Duration) -> bool {
        self.wait_for(|state| state.returned >= count, timeout)
    }
    fn wait_for(&self, predicate: impl Fn(&GateState) -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        while !predicate(&state) {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, result) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
            if result.timed_out() && !predicate(&state) {
                return false;
            }
        }
        true
    }
}

struct Resolver;
impl PathResolver for Resolver {
    fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
        if path.exists() {
            path.canonicalize()
        } else {
            Ok(self
                .resolve_existing_ancestor(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?
                .join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?))
        }
    }
}
struct FactoryState {
    _temp: tempfile::TempDir,
    root: provenant_core::StateRoot,
    clock: TestClock,
    backoff: ThreadWait,
    gate: Arc<Gate>,
}
#[derive(Clone)]
struct FakeFactory(Arc<FactoryState>);
impl FakeFactory {
    fn new(gate: Arc<Gate>) -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut inputs = RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let root = inputs.resolve(&Resolver).unwrap().state;
        let clock = TestClock::new(
            WallTime::from_micros(2_000_000_000_000_000).unwrap(),
            MonoTime::from_micros(0),
        );
        let state = Arc::new(FactoryState {
            _temp: temp,
            root,
            clock,
            backoff: ThreadWait,
            gate,
        });
        let budget = WaitBudget::mcp(state.clock.mono_now(), 5).unwrap();
        let _initialised = CoordinationService::open(
            &state.root,
            ProjectId::from_bytes([1; 16]),
            ActorId::from_bytes([2; 16]),
            &state.clock,
            &state.backoff,
            budget,
            &CancelToken::new(),
        )
        .unwrap();
        Self(state)
    }
}
#[derive(Serialize)]
struct Fact {
    state: &'static str,
}
struct FakeFacade(Arc<Gate>);
impl F3Facade for FakeFacade {
    type Identity = Fact;
    type Submit = Fact;
    type Run = Fact;
    type Hosts = Fact;
    fn identity(
        &mut self,
        _: IdentityInput,
        _: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        self.0.maybe_panic();
        self.0.hold(cancel);
        Err(FacadeError::NotFound)
    }
    fn submit(
        &mut self,
        _: SubmitInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        Ok(Fact { state: "accepted" })
    }
    fn run(
        &mut self,
        input: RunInput,
        _: WaitBudget,
        cancel: &CancelToken,
    ) -> Result<Fact, FacadeError> {
        if matches!(input, RunInput::Cancel { .. }) {
            self.0.run_cancel_call();
        }
        self.0.hold(cancel);
        Ok(Fact { state: "running" })
    }
    fn output(
        &mut self,
        input: OutputInput,
        _: WaitBudget,
        _: &CancelToken,
    ) -> Result<OutputResult, FacadeError> {
        let OutputInput::Fetch {
            target, artifact, ..
        } = input
        else {
            return Err(FacadeError::NotFound);
        };
        Ok(OutputResult::Artifact {
            target,
            artifact,
            digest: provenant_core::Digest::from_bytes([1; 32]),
            bytes: OutputBytes::from_bytes(self.0.state.lock().unwrap().output_bytes.clone()),
        })
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
impl Factory for FakeFactory {
    type Facade<'a>
        = FakeFacade
    where
        Self: 'a;
    fn open(&self) -> Result<Service<'_, Self::Facade<'_>>, ServiceError> {
        let budget = WaitBudget::mcp(self.0.clock.mono_now(), 5)?;
        let coordination = CoordinationService::open(
            &self.0.root,
            ProjectId::from_bytes([1; 16]),
            ActorId::from_bytes([2; 16]),
            &self.0.clock,
            &self.0.backoff,
            budget,
            &CancelToken::new(),
        )?;
        Ok(Service::new(
            coordination,
            FakeFacade(self.0.gate.clone()),
            &self.0.clock,
        ))
    }
}

#[derive(Default)]
struct Shared {
    input: Mutex<InputState>,
    input_changed: Condvar,
    output: Mutex<OutputState>,
    output_changed: Condvar,
}
#[derive(Default)]
struct InputState {
    bytes: VecDeque<u8>,
    eof: bool,
    consumed: usize,
}
#[derive(Default)]
struct OutputState {
    frames: Vec<Vec<u8>>,
    broken: bool,
    delay: Duration,
}
struct PipeInput(Arc<Shared>);
struct PipeOutput(Arc<Shared>);
impl Input for PipeInput {
    fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize> {
        let mut input = self
            .0
            .input
            .lock()
            .map_err(|_| io::Error::other("test input lock poisoned"))?;
        while input.bytes.is_empty() && !input.eof {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            let (next, timeout) = self
                .0
                .input_changed
                .wait_timeout(input, remaining)
                .map_err(|_| io::Error::other("test input lock poisoned"))?;
            input = next;
            if timeout.timed_out() && input.bytes.is_empty() && !input.eof {
                return Err(io::ErrorKind::WouldBlock.into());
            }
        }
        if input.bytes.is_empty() && input.eof {
            return Ok(0);
        }
        let size = bytes.len().min(input.bytes.len());
        for byte in &mut bytes[..size] {
            *byte = input
                .bytes
                .pop_front()
                .ok_or_else(|| io::Error::other("test input queue changed"))?;
        }
        input.consumed += size;
        self.0.input_changed.notify_all();
        Ok(size)
    }
}
impl Output for PipeOutput {
    fn write_frame(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()> {
        let delay = {
            let output = self
                .0
                .output
                .lock()
                .map_err(|_| io::Error::other("test output lock poisoned"))?;
            if output.broken {
                return Err(io::ErrorKind::BrokenPipe.into());
            }
            output.delay
        };
        provenant_fabric_runtime::coordination::RetryWait::wait(
            &ThreadWait,
            provenant_core::DurationMicros::from_micros(u64::try_from(delay.as_micros()).unwrap()),
            &CancelToken::new(),
        );
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let mut output = self
            .0
            .output
            .lock()
            .map_err(|_| io::Error::other("test output lock poisoned"))?;
        if output.broken {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        output.frames.push(bytes.to_vec());
        self.0.output_changed.notify_all();
        Ok(())
    }
}
struct Server {
    shared: Arc<Shared>,
    gate: Arc<Gate>,
    result: Option<mpsc::Receiver<Result<(), Error>>>,
}
impl Server {
    fn start(factory: FakeFactory, broken_output: bool) -> Self {
        Self::start_with_timeouts(factory, broken_output, TIMEOUTS)
    }
    fn start_with_timeouts(factory: FakeFactory, broken_output: bool, timeouts: Timeouts) -> Self {
        let shared = Arc::new(Shared::default());
        shared.output.lock().unwrap().broken = broken_output;
        let gate = factory.0.gate.clone();
        let (send, result) = mpsc::sync_channel(1);
        let (mut reader, mut writer) = (PipeInput(shared.clone()), PipeOutput(shared.clone()));
        thread::spawn(move || {
            let _ = send.send(serve_with_timeouts(
                &mut reader,
                &mut writer,
                factory,
                timeouts,
            ));
        });
        Self {
            shared,
            gate,
            result: Some(result),
        }
    }
    fn send(&self, value: &Value) {
        self.send_bytes(&encode_line(value));
    }
    fn send_bytes(&self, bytes: &[u8]) {
        self.shared
            .input
            .lock()
            .unwrap()
            .bytes
            .extend(bytes.iter().copied());
        self.shared.input_changed.notify_all();
    }
    fn close_input(&self) {
        self.shared.input.lock().unwrap().eof = true;
        self.shared.input_changed.notify_all();
    }
    fn wait_for_consumed(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut input = self.shared.input.lock().unwrap();
        while input.consumed < count {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            let (next, result) = self
                .shared
                .input_changed
                .wait_timeout(input, remaining)
                .unwrap();
            input = next;
            if result.timed_out() && input.consumed < count {
                return false;
            }
        }
        true
    }
    fn wait_for_frames(&self, count: usize, timeout: Duration) -> Option<Vec<Value>> {
        let deadline = Instant::now() + timeout;
        let mut output = self.shared.output.lock().unwrap();
        while output.frames.len() < count {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let (next, result) = self
                .shared
                .output_changed
                .wait_timeout(output, remaining)
                .unwrap();
            output = next;
            if result.timed_out() && output.frames.len() < count {
                return None;
            }
        }
        Some(
            output
                .frames
                .iter()
                .map(|frame| serde_json::from_slice(frame).unwrap())
                .collect(),
        )
    }
    fn captured_frames(&self) -> Vec<Value> {
        self.shared
            .output
            .lock()
            .unwrap()
            .frames
            .iter()
            .map(|frame| serde_json::from_slice(frame).unwrap())
            .collect()
    }
    fn wait_result(&self, timeout: Duration) -> Result<(), Error> {
        self.result.as_ref().unwrap().recv_timeout(timeout).unwrap()
    }
    fn no_more_frames(&self, count: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut output = self.shared.output.lock().unwrap();
        while output.frames.len() == count {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return true;
            };
            let (next, result) = self
                .shared
                .output_changed
                .wait_timeout(output, remaining)
                .unwrap();
            output = next;
            if result.timed_out() {
                return output.frames.len() == count;
            }
        }
        output.frames.len() == count
    }
    fn finish(mut self) -> Result<(), Error> {
        self.gate.release();
        self.close_input();
        self.result.take().unwrap().recv_timeout(TEST_WAIT).unwrap()
    }
    fn finish_with_frames(mut self) -> (Result<(), Error>, Vec<Value>) {
        self.gate.release();
        self.close_input();
        let result = self.result.take().unwrap().recv_timeout(TEST_WAIT).unwrap();
        (result, self.captured_frames())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.gate.release();
        self.close_input();
    }
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":protocol::LEGACY,"capabilities":{},"clientInfo":{"name":"stdio test","version":"1"}}})
}
fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn tool_call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}
fn encode_line(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    bytes
}
