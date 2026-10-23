#![allow(clippy::panic, clippy::unwrap_used)]

use super::*;
use crate::tools::{
    FabricService, FacadeError, HostsInput, IdentityInput, OutputInput, RunInput, ServiceError,
    SubmitInput, ToolResponse,
};
use provenant_core::CancelToken;
use provenant_fabric_runtime::coordination::{
    ClaimInput, EventsInput, LandingInput, MailInput, TaskInput,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io,
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

const TEST_WAIT: Duration = Duration::from_secs(3);

#[test]
fn framer_keeps_utf8_bytes_across_chunks_and_frames() {
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
fn framer_accepts_the_limit_and_rejects_one_more_byte() {
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

    let mut overlong_frame = Framer::default();
    overlong_frame.push(&vec![b'x'; MAX_FRAME + 1]).unwrap();
    assert!(matches!(overlong_frame.next_frame(), Err(Error::Oversize)));
    assert!(matches!(overlong_frame.push(b"x"), Err(Error::Oversize)));
}

#[test]
fn framer_reports_an_incomplete_eof_candidate() {
    let mut framer = Framer::default();
    framer.push(br#"{"jsonrpc":"2.0"}"#).unwrap();
    assert!(framer.incomplete());
    assert_eq!(framer.next_frame().unwrap(), None);
}

#[test]
fn serve_splits_utf8_safely_and_keeps_tool_errors_inside_result() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    let init = initialize();
    let initialized = notification("notifications/initialized", json!({}));
    let prefix_len = encode_line(&init).len() + encode_line(&initialized).len();
    server.send(&init);
    server.send(&initialized);
    let line = encode_line(&json!({
        "jsonrpc":"2.0", "id":"unicode", "method":"tools/call",
        "params":{"name":"fabric_events","arguments":{"op":"note","note":"café 🦜"}}
    }));
    let split = line
        .windows(2)
        .position(|window| window == [0xc3, 0xa9])
        .map(|index| index + 1)
        .unwrap();
    server.send_bytes(&line[..split]);
    assert!(server.wait_for_consumed(prefix_len + split, TEST_WAIT));
    server.send_bytes(&line[split..]);

    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["id"], "init");
    assert_eq!(frames[1]["id"], "unicode");
    assert!(frames[1]["result"]["isError"].as_bool().unwrap());
    assert!(frames[1].get("error").is_none());
    assert_eq!(
        frames[1]["result"]["structuredContent"]["code"],
        "not_found"
    );
    assert_eq!(gate.unicode(), Some("café 🦜".to_owned()));
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn serve_reports_malformed_deep_unknown_tool_and_unknown_fields() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    let mut bytes = b"{\n".to_vec();
    let nested = format!("{}0{}", "[".repeat(80), "]".repeat(80));
    let deep = format!(
        r#"{{"jsonrpc":"2.0","id":"deep","method":"tools/list","params":{{"nest":{nested}}}}}"#
    );
    bytes.extend_from_slice(deep.as_bytes());
    bytes.push(b'\n');
    bytes.extend_from_slice(&encode_line(&initialize()));
    bytes.extend_from_slice(&encode_line(&notification(
        "notifications/initialized",
        json!({}),
    )));
    bytes.extend_from_slice(&encode_line(&tool_call(
        "unknown",
        "fabric_missing",
        json!({}),
    )));
    bytes.extend_from_slice(&encode_line(&tool_call(
        "extra-field",
        "fabric_identity",
        json!({"op":"inspect","extra":true}),
    )));
    server.send_bytes(&bytes);
    // If strict deserialisation regresses, let the fake service return so the
    // assertion reports the wrong wire response instead of timing out.
    gate.release();

    let frames = server
        .wait_for_frames(5, TEST_WAIT)
        .unwrap_or_else(|| panic!("only received frames: {:?}", server.captured_frames()));
    assert_eq!(frames[0]["error"]["code"], -32700);
    assert_eq!(frames[1]["error"]["code"], -32700);
    assert_eq!(frames[2]["id"], "init");
    assert_eq!(frames[3]["error"]["code"], -32602);
    assert_eq!(frames[3]["id"], "unknown");
    assert_eq!(frames[4]["error"]["code"], -32602);
    assert_eq!(frames[4]["id"], "extra-field");
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn serve_refuses_out_of_order_lifecycle_and_duplicate_live_ids() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&json!({"jsonrpc":"2.0","id":"early","method":"tools/list"}));
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    server.send(&tool_call(
        "duplicate",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    assert!(gate.wait_for_entered(1, TEST_WAIT));
    server.send(&tool_call(
        "duplicate",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));

    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["error"]["code"], -32600);
    assert_eq!(frames[0]["id"], "early");
    assert_eq!(frames[1]["id"], "init");
    assert_eq!(frames[2]["error"]["code"], -32600);
    assert_eq!(frames[2]["error"]["message"], "Duplicate live request ID");
    assert_eq!(frames[2]["id"], "duplicate");

    gate.release();
    assert!(gate.wait_for_returned(1, TEST_WAIT));
    assert!(matches!(server.finish(), Ok(())));
}

#[test]
fn cancellation_suppresses_late_run_status_without_a_run_cancel_call() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    let target = json!({"attempt":"00000000000000000000000000000001"});
    server.send(&tool_call(
        "late",
        "fabric_run",
        json!({"op":"status","target":target}),
    ));
    assert!(gate.wait_for_entered(1, TEST_WAIT));
    server.send(&notification(
        "notifications/cancelled",
        json!({"requestId":"late"}),
    ));
    // This reply proves the cancellation notification was handled before the
    // blocked tool is allowed to return.
    server.send(&json!({"jsonrpc":"2.0","id":"barrier","method":"tools/list"}));
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[0]["id"], "init");
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
fn four_workers_and_queue_capacity_return_busy_after_thirty_six_calls() {
    let gate = Arc::new(Gate::default());
    let server = Server::start(FakeFactory::new(gate.clone()), false);
    server.send(&initialize());
    server.send(&notification("notifications/initialized", json!({})));
    assert!(server.wait_for_frames(1, TEST_WAIT).is_some());

    let mut calls = Vec::new();
    for id in 0..4 {
        calls.extend_from_slice(&encode_line(&tool_call(
            &id.to_string(),
            "fabric_identity",
            json!({"op":"inspect"}),
        )));
    }
    server.send_bytes(&calls);
    assert!(gate.wait_for_entered(4, TEST_WAIT));

    calls.clear();
    for id in 4..36 {
        calls.extend_from_slice(&encode_line(&tool_call(
            &id.to_string(),
            "fabric_identity",
            json!({"op":"inspect"}),
        )));
    }
    calls.extend_from_slice(&encode_line(&json!({
        "jsonrpc":"2.0","id":"queue-barrier","method":"tools/list"
    })));
    server.send_bytes(&calls);
    let frames = server.wait_for_frames(2, TEST_WAIT).unwrap();
    assert_eq!(frames[1]["id"], "queue-barrier");

    server.send(&tool_call(
        "overflow",
        "fabric_identity",
        json!({"op":"inspect"}),
    ));
    let frames = server.wait_for_frames(3, TEST_WAIT).unwrap();
    assert_eq!(frames[2]["id"], "overflow");
    assert!(frames[2]["result"]["isError"].as_bool().unwrap());
    assert_eq!(
        frames[2]["result"]["structuredContent"]["error"]["code"],
        "busy"
    );

    gate.release();
    assert!(gate.wait_for_returned(36, TEST_WAIT));
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
    // MCP 2026-07-28 schema#jsonrpcerrorresponse: optional id is RequestId.
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
fn oversize_frame_fails_transport_and_broken_stdout_is_reported() {
    let server = Server::start(FakeFactory::new(Arc::new(Gate::default())), false);
    server.send_bytes(&vec![b'x'; MAX_FRAME + 1]);
    assert!(matches!(server.finish(), Err(Error::Oversize)));

    let broken = Server::start(FakeFactory::new(Arc::new(Gate::default())), true);
    broken.send_bytes(b"{\n");
    assert!(
        matches!(broken.finish(), Err(Error::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe)
    );
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
    run_cancel_calls: usize,
    unicode: Option<String>,
}

impl Gate {
    fn hold(&self) {
        let mut state = self.state.lock().unwrap();
        state.entered += 1;
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

    fn record_unicode(&self, value: String) {
        self.state.lock().unwrap().unicode = Some(value);
    }

    fn unicode(&self) -> Option<String> {
        self.state.lock().unwrap().unicode.clone()
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

#[derive(Clone)]
struct FakeFactory {
    gate: Arc<Gate>,
}

impl FakeFactory {
    fn new(gate: Arc<Gate>) -> Self {
        Self { gate }
    }
}

impl Factory for FakeFactory {
    type Service<'a>
        = FakeService
    where
        Self: 'a;

    fn open(&self) -> Result<Self::Service<'_>, ServiceError> {
        Ok(FakeService {
            gate: self.gate.clone(),
        })
    }
}

struct FakeService {
    gate: Arc<Gate>,
}

fn missing() -> Result<ToolResponse, ServiceError> {
    Err(ServiceError::Facade(FacadeError::NotFound))
}

macro_rules! unused_calls {
    ($($method:ident: $input:ty),+ $(,)?) => {
        $(
            fn $method(
                &mut self,
                _input: $input,
                _cancel: &CancelToken,
            ) -> Result<ToolResponse, ServiceError> {
                missing()
            }
        )+
    };
}

impl FabricService for FakeService {
    fn identity(
        &mut self,
        _input: IdentityInput,
        _cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        self.gate.hold();
        missing()
    }

    fn events(
        &mut self,
        input: EventsInput,
        _cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        if let EventsInput::Note { note, .. } = input {
            self.gate.record_unicode(note);
        }
        missing()
    }

    fn run(
        &mut self,
        input: RunInput,
        _cancel: &CancelToken,
    ) -> Result<ToolResponse, ServiceError> {
        if matches!(input, RunInput::Cancel { .. }) {
            self.gate.run_cancel_call();
        }
        self.gate.hold();
        missing()
    }

    unused_calls! {
        mail: MailInput,
        task: TaskInput,
        claim: ClaimInput,
        landing: LandingInput,
        submit: SubmitInput,
        output: OutputInput,
        hosts: HostsInput,
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
        let shared = Arc::new(Shared::default());
        shared.output.lock().unwrap().broken = broken_output;
        let gate = factory.gate.clone();
        let (send_result, result) = mpsc::sync_channel(1);
        let (reader, writer) = (PipeInput(shared.clone()), PipeOutput(shared.clone()));
        thread::spawn(move || {
            let mut reader = reader;
            let mut writer = writer;
            let _ = send_result.send(serve(&mut reader, &mut writer, factory));
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
        let mut input = self.shared.input.lock().unwrap();
        input.bytes.extend(bytes.iter().copied());
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
}

impl Drop for Server {
    fn drop(&mut self) {
        self.gate.release();
        self.close_input();
    }
}

fn initialize() -> Value {
    json!({
        "jsonrpc":"2.0", "id":"init", "method":"initialize",
        "params":{"protocolVersion":crate::protocol::LEGACY,"capabilities":{},"clientInfo":{"name":"stdio test","version":"1"}}
    })
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
