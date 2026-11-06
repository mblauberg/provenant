//! Attempt-bound host coordination relay. Sender authority stays in the host.
#![allow(clippy::disallowed_methods)] // Native socket I/O uses real deadlines.
use crate::tools::{FacadeError, ServiceError, ToolCall};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Parallel host owners per attempt, matching the MCP worker bound.
pub const WORKERS: usize = 4;

/// Reserve response delivery time without extending a caller's deadline.
pub fn host_budget(
    clock: &dyn Clock,
    within: Duration,
) -> Result<WaitBudget, provenant_core::TimeError> {
    let now = clock.mono_now();
    let within = within
        .min(Duration::from_secs(crate::MAX_WAIT_SECONDS))
        .saturating_sub(Duration::from_millis(100));
    Ok(WaitBudget::mcp(now, crate::MAX_WAIT_SECONDS)?
        .cap_at(now.checked_add(DurationMicros::from_micros(within.as_micros() as u64))?))
}

/// Relay frame for a lane adding to its own graph. It is not an MCP tool:
/// the host resolves the parent node from the attempt the relay serves.
pub const GRAPH_TOOL: &str = "graph";

/// Host handler for graph frames: arguments, cancellation, caller budget.
pub trait GraphHandler:
    Fn(Value, &CancelToken, Duration) -> Result<Value, ServiceError> + Sync
{
}
impl<T: Fn(Value, &CancelToken, Duration) -> Result<Value, ServiceError> + Sync> GraphHandler
    for T
{
}

fn no_graph(_: Value, _: &CancelToken, _: Duration) -> Result<Value, ServiceError> {
    Err(ServiceError::Cli {
        code: "graph_relay_unavailable".into(),
        message: "this attempt is not a graph node".into(),
        data: Value::Null,
    })
}

const MAX_BYTES: u64 = 1_048_576;
const IO_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub tool: String,
    pub arguments: Value,
}
#[derive(Serialize, Deserialize)]
enum Reply {
    Ready,
    Ok(Value),
    Error {
        code: String,
        detail: Value,
        cause: Option<FacadeError>,
        operation_id: Option<provenant_core::OperationId>,
    },
}

/// The delivery variants project onto the established IO envelope on the
/// relay as they do on the peer wire; their typed facts remain available.
fn relay_cause(error: &FacadeError) -> FacadeError {
    match error {
        FacadeError::HostDeliveryLimit { fact } => FacadeError::Io {
            operation: "host delivery".into(),
            path: None,
            delivery_limit: Some(fact.clone()),
            delivery_stage: None,
            resource: None,
            input_failure: None,
        },
        FacadeError::HostDeliveryStage { stage } => FacadeError::Io {
            operation: "host delivery".into(),
            path: None,
            delivery_limit: None,
            delivery_stage: Some(*stage),
            resource: None,
            input_failure: None,
        },
        _ => error.clone(),
    }
}

fn service_cause(error: &ServiceError) -> Option<FacadeError> {
    match error {
        ServiceError::Facade(cause) => Some(relay_cause(cause)),
        ServiceError::Coordination(cause) => Some(relay_cause(&FacadeError::from(cause))),
        ServiceError::Invalid(cause) => Some(FacadeError::from(
            &provenant_fabric_runtime::coordination::CoordinationError::Input(cause.clone()),
        )),
        ServiceError::Mutation { source, .. } => service_cause(source),
        _ => None,
    }
}

fn with_operation(
    error: ServiceError,
    operation_id: Option<provenant_core::OperationId>,
) -> ServiceError {
    match operation_id {
        Some(operation_id) => ServiceError::Mutation {
            operation_id,
            source: Box::new(error),
        },
        None => error,
    }
}

/// The guardian owns this handle until provider exit, including failed spawn.
pub struct HostRelay {
    path: PathBuf,
    cancel: CancelToken,
    thread: Option<JoinHandle<()>>,
}
impl HostRelay {
    /// The endpoint must have a new private parent beneath a host-controlled root.
    pub fn start(
        path: PathBuf,
        limits: provenant_records::Limits,
        handler: impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError>
        + Send
        + Sync
        + 'static,
    ) -> io::Result<Self> {
        Self::start_with_graph(path, limits, handler, no_graph)
    }

    /// As [`Self::start`], also serving [`GRAPH_TOOL`] frames, through which
    /// a lane adds children to the graph it runs in.
    pub fn start_with_graph(
        path: PathBuf,
        limits: provenant_records::Limits,
        handler: impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError>
        + Send
        + Sync
        + 'static,
        graph: impl GraphHandler + Send + 'static,
    ) -> io::Result<Self> {
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        match std::fs::DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = std::fs::symlink_metadata(parent)?;
                if !metadata.file_type().is_dir()
                    || metadata.uid() != provenant_os::fs::effective_uid()
                    || metadata.mode() & 0o7777 != 0o700
                {
                    return Err(error);
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                std::fs::remove_dir(parent)?;
                std::fs::DirBuilder::new().mode(0o700).create(parent)?;
            }
            Err(error) => return Err(error),
        }
        let mut relay = Self {
            path,
            cancel: CancelToken::new(),
            thread: None,
        };
        let listener = UnixListener::bind(&relay.path)?;
        std::fs::set_permissions(&relay.path, std::fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let cancel = relay.cancel.clone();
        relay.thread = Some(thread::Builder::new().name("fabric-relay".into()).spawn(
            move || {
                serve_connections(limits, &handler, &graph, &cancel, |send| {
                    while !cancel.is_cancelled() {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if let Err(mpsc::TrySendError::Full(mut stream)) =
                                    send.try_send(stream)
                                {
                                    let _ = write(
                                        &mut stream,
                                        &Reply::Error {
                                            code: "busy".into(),
                                            detail: Value::Null,
                                            cause: None,
                                            operation_id: None,
                                        },
                                        Instant::now() + IO_TIMEOUT,
                                        &cancel,
                                    );
                                }
                            }
                            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                                thread::park_timeout(Duration::from_millis(20))
                            }
                            Err(_) => break,
                        }
                    }
                });
            },
        )?);
        Ok(relay)
    }
}
fn serve_connections(
    limits: provenant_records::Limits,
    handler: &(impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError> + Sync),
    graph: &impl GraphHandler,
    cancel: &CancelToken,
    accept: impl FnOnce(mpsc::SyncSender<UnixStream>),
) {
    let (send, receive) = mpsc::sync_channel::<UnixStream>(32);
    let receive = Mutex::new(receive);
    thread::scope(|scope| {
        for _ in 0..WORKERS {
            let receive = &receive;
            scope.spawn(move || {
                while !cancel.is_cancelled() {
                    let received = match receive.lock() {
                        Ok(receive) => receive.recv_timeout(Duration::from_millis(20)),
                        Err(_) => break,
                    };
                    match received {
                        Ok(mut stream) => {
                            let _ = serve_one_with(&mut stream, limits, handler, graph, cancel);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
        }
        accept(send);
    });
}

impl Drop for HostRelay {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

fn remaining(deadline: Instant, cancel: &CancelToken) -> io::Result<Duration> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(remaining.min(Duration::from_millis(100)))
}
fn read_bytes(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(deadline, cancel)?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(size) => bytes = &mut bytes[size..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                thread::park_timeout(remaining(deadline, cancel)?.min(Duration::from_millis(10)))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn read(
    stream: &mut UnixStream,
    max_bytes: u64,
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<Vec<u8>> {
    let mut size = [0; 4];
    read_bytes(stream, &mut size, deadline, cancel)?;
    let size = u32::from_be_bytes(size);
    if u64::from(size) > max_bytes.min(MAX_BYTES) {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; size as usize];
    read_bytes(stream, &mut bytes, deadline, cancel)?;
    Ok(bytes)
}
fn write(
    stream: &mut UnixStream,
    value: &impl Serialize,
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut frame = (bytes.len() as u32).to_be_bytes().to_vec();
    frame.extend(bytes);
    write_bytes(stream, &frame, deadline, cancel)
}

fn write_bytes(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<()> {
    while !bytes.is_empty() {
        remaining(deadline, cancel)?;
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(size) => bytes = &bytes[size..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                ) =>
            {
                thread::park_timeout(remaining(deadline, cancel)?.min(Duration::from_millis(10)))
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn serve_one(
    stream: &mut UnixStream,
    limits: provenant_records::Limits,
    handler: &impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError>,
    cancel: &CancelToken,
) -> io::Result<()> {
    serve_one_with(stream, limits, handler, &no_graph, cancel)
}

/// A queued caller must still be present and confirm its remaining budget
/// before any effect. A timed-out queued call cannot execute.
fn confirm_budget(
    stream: &mut UnixStream,
    deadline: Instant,
    cancel: &CancelToken,
) -> io::Result<Duration> {
    write(stream, &Reply::Ready, deadline, cancel)?;
    let mut milliseconds = [0; 4];
    read_bytes(
        stream,
        &mut milliseconds,
        Instant::now() + IO_TIMEOUT,
        cancel,
    )?;
    let milliseconds =
        u64::from(u32::from_be_bytes(milliseconds)).min(crate::MAX_WAIT_SECONDS * 1000);
    if milliseconds == 0 {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(Duration::from_millis(milliseconds))
}

fn reply(result: Result<Value, ServiceError>) -> Reply {
    match result {
        Ok(value) => Reply::Ok(value),
        Err(error) => Reply::Error {
            code: error.code().into(),
            detail: error.structured_error(),
            cause: service_cause(&error),
            operation_id: match &error {
                ServiceError::Mutation { operation_id, .. } => Some(operation_id.clone()),
                _ => None,
            },
        },
    }
}

pub(crate) fn serve_one_with(
    stream: &mut UnixStream,
    limits: provenant_records::Limits,
    handler: &impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError>,
    graph: &impl GraphHandler,
    cancel: &CancelToken,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let bytes = read(stream, limits.max_bytes, deadline, cancel)?;
    let request: Request = provenant_records::decode(bytes.as_slice(), limits)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let reply = if request.tool == GRAPH_TOOL {
        let within = confirm_budget(stream, deadline, cancel)?;
        reply(graph(request.arguments, cancel, within))
    } else {
        match ToolCall::parse(&request.tool, request.arguments) {
            Ok(call) => {
                let within = confirm_budget(stream, deadline, cancel)?;
                reply(handler(call, cancel, within))
            }
            Err(crate::tools::InvalidInput::Validated(error)) => Reply::Error {
                code: "invalid_input".into(),
                detail: ServiceError::Invalid(error.clone()).structured_error(),
                cause: Some(FacadeError::from(
                    &provenant_fabric_runtime::coordination::CoordinationError::Input(error),
                )),
                operation_id: None,
            },
            Err(_) => Reply::Error {
                code: "invalid_input".into(),
                detail: Value::Null,
                cause: None,
                operation_id: None,
            },
        }
    };
    let deadline = Instant::now() + IO_TIMEOUT;
    if serde_json::to_vec(&reply)?.len() as u64 > MAX_BYTES {
        return write(
            stream,
            &Reply::Error {
                code: "response_too_large".into(),
                detail: Value::Null,
                cause: None,
                operation_id: None,
            },
            deadline,
            cancel,
        );
    }
    write(stream, &reply, deadline, cancel)
}

pub(crate) fn mutate(
    path: &Path,
    request: &Request,
    cancel: &CancelToken,
    within: Duration,
) -> Result<Value, ServiceError> {
    let mut stream = UnixStream::connect(path).map_err(|_| ServiceError::FabricOwnerUnavailable)?;
    exchange_within(&mut stream, request, cancel, within)
}
/// Send a graph frame from a lane: add a child, or mark the caller waiting.
/// The host answers with a receipt and journals the change before replying.
pub fn graph(
    path: &Path,
    arguments: Value,
    cancel: &CancelToken,
    within: Duration,
) -> Result<Value, ServiceError> {
    mutate(
        path,
        &Request {
            tool: GRAPH_TOOL.into(),
            arguments,
        },
        cancel,
        within,
    )
}

#[cfg(test)]
pub(crate) fn exchange(
    stream: &mut UnixStream,
    request: &Request,
    cancel: &CancelToken,
) -> Result<Value, ServiceError> {
    exchange_within(
        stream,
        request,
        cancel,
        Duration::from_secs(crate::MAX_WAIT_SECONDS),
    )
}

pub(crate) fn exchange_within(
    stream: &mut UnixStream,
    request: &Request,
    cancel: &CancelToken,
    within: Duration,
) -> Result<Value, ServiceError> {
    let unavailable = |error: io::Error| {
        if cancel.is_cancelled() {
            ServiceError::Facade(FacadeError::Cancelled)
        } else if error.kind() == io::ErrorKind::TimedOut {
            ServiceError::Facade(FacadeError::Timeout)
        } else {
            ServiceError::FabricOwnerUnavailable
        }
    };
    stream.set_nonblocking(true).map_err(unavailable)?;
    let deadline = Instant::now() + within.min(Duration::from_secs(crate::MAX_WAIT_SECONDS));
    write(
        stream,
        request,
        deadline.min(Instant::now() + IO_TIMEOUT),
        cancel,
    )
    .map_err(unavailable)?;
    let reply =
        serde_json::from_slice(&read(stream, MAX_BYTES, deadline, cancel).map_err(unavailable)?)?;
    let reply = if matches!(reply, Reply::Ready) {
        let milliseconds = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as u32;
        if milliseconds == 0 || cancel.is_cancelled() {
            return Err(FacadeError::Timeout.into());
        }
        write_bytes(
            stream,
            &milliseconds.to_be_bytes(),
            deadline.min(Instant::now() + IO_TIMEOUT),
            cancel,
        )
        .map_err(unavailable)?;
        serde_json::from_slice(&read(stream, MAX_BYTES, deadline, cancel).map_err(unavailable)?)?
    } else {
        reply
    };
    match reply {
        Reply::Ok(value) => Ok(value),
        Reply::Error {
            code,
            detail,
            cause,
            operation_id,
        } => {
            if let Some(cause) = cause.filter(|cause| cause.code() == code) {
                return Err(with_operation(cause.into(), operation_id));
            }
            if code == "result_too_large" || code == "response_too_large" {
                return Err(with_operation(ServiceError::ResultTooLarge, operation_id));
            }
            if (detail.is_null()
                || detail.as_object().is_some_and(|fields| {
                    fields.keys().all(|key| {
                        matches!(key.as_str(), "code" | "message" | "hint" | "retryable")
                            || (matches!(code.as_str(), "busy" | "conflict" | "cancelled")
                                && matches!(key.as_str(), "category" | "detail" | "next"))
                    })
                }))
                && let Ok(error) =
                    serde_json::from_value::<FacadeError>(serde_json::json!({"code": code}))
            {
                return Err(with_operation(error.into(), operation_id));
            }
            let message = detail
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("host Fabric coordination refused")
                .to_owned();
            Err(with_operation(
                ServiceError::Cli {
                    code,
                    message,
                    data: detail,
                },
                operation_id,
            ))
        }
        Reply::Ready => Err(FacadeError::StateCorrupt.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn request() -> Request {
        Request {
            tool: "fabric_events".into(),
            arguments: serde_json::json!({"op":"note","note":"queued"}),
        }
    }

    #[test]
    fn relay_drop_cancels_handler_and_next_call_is_unavailable()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::Builder::new().prefix("pf-").tempdir_in(".")?;
        let path = temp
            .path()
            .strip_prefix(std::env::current_dir()?)?
            .join("relay/socket");
        let (started, ready) = mpsc::sync_channel(1);
        let relay = HostRelay::start(
            path.clone(),
            provenant_records::Limits::CONFIG,
            move |_, cancel, _| {
                started.send(()).unwrap();
                provenant_test_support::wait_for(
                    "relay cancellation",
                    IO_TIMEOUT,
                    || cancel.is_cancelled(),
                    |cancelled| *cancelled,
                )
                .unwrap();
                Err(FacadeError::Cancelled.into())
            },
        )?;
        thread::scope(|scope| {
            let path = path.clone();
            let call = scope.spawn(move || {
                mutate(
                    &path,
                    &request(),
                    &CancelToken::new(),
                    Duration::from_secs(5),
                )
            });
            ready.recv_timeout(IO_TIMEOUT).unwrap();
            let start = Instant::now();
            drop(relay);
            assert!(start.elapsed() < IO_TIMEOUT);
            assert!(call.join().unwrap().is_err());
        });
        let start = Instant::now();
        let error = mutate(
            &path,
            &request(),
            &CancelToken::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, ServiceError::FabricOwnerUnavailable));
        assert!(
            error
                .to_string()
                .contains("host Fabric endpoint unavailable")
        );
        assert!(start.elapsed() < IO_TIMEOUT);
        Ok(())
    }

    #[test]
    fn host_errors_keep_codes_messages_and_retryability() -> Result<(), Box<dyn std::error::Error>>
    {
        for error in [
            FacadeError::Busy,
            FacadeError::Conflict,
            FacadeError::Cancelled,
        ] {
            let (mut client, mut server) = UnixStream::pair()?;
            thread::scope(|scope| {
                let expected = error.clone();
                let serving = scope.spawn(move || {
                    serve_one(
                        &mut server,
                        provenant_records::Limits::CONFIG,
                        &|_, _, _| Err(expected.clone().into()),
                        &CancelToken::new(),
                    )
                });
                let received = exchange(&mut client, &request(), &CancelToken::new()).unwrap_err();
                assert!(matches!(&received, ServiceError::Facade(actual) if *actual == error));
                if error == FacadeError::Busy {
                    assert_eq!(received.structured_error()["retryable"], true);
                }
                serving.join().unwrap().unwrap();
            });
        }
        Ok(())
    }

    #[test]
    fn graph_frames_reach_the_graph_handler_with_codes_kept()
    -> Result<(), Box<dyn std::error::Error>> {
        let graph_handler = |arguments: Value, _: &CancelToken, within: Duration| {
            assert!(!within.is_zero());
            if arguments["id"] == "deep" {
                return Err(ServiceError::Cli {
                    code: "child_depth".into(),
                    message: "limits.child_depth=8".into(),
                    data: Value::Null,
                });
            }
            let id = arguments["id"].as_str().unwrap_or_default();
            Ok(serde_json::json!({"node": format!("a--{id}"), "replayed": false}))
        };
        let call = |id: &str,
                    graph_handler: &(
                         dyn Fn(Value, &CancelToken, Duration) -> Result<Value, ServiceError> + Sync
                     )| {
            let (mut client, mut server) = UnixStream::pair()?;
            thread::scope(|scope| {
                let serving = scope.spawn(|| {
                    serve_one_with(
                        &mut server,
                        provenant_records::Limits::CONFIG,
                        &|_, _, _| Err(FacadeError::StateCorrupt.into()),
                        &|arguments, cancel: &CancelToken, within| {
                            graph_handler(arguments, cancel, within)
                        },
                        &CancelToken::new(),
                    )
                });
                let request = Request {
                    tool: GRAPH_TOOL.into(),
                    arguments: serde_json::json!({"op":"add","id":id}),
                };
                let result = exchange(&mut client, &request, &CancelToken::new());
                serving.join().unwrap().unwrap();
                Ok::<_, io::Error>(result)
            })
        };
        assert_eq!(call("fix", &graph_handler)??["node"], "a--fix");
        assert_eq!(
            call("deep", &graph_handler)?.unwrap_err().code(),
            "child_depth"
        );
        // A relay without a graph refuses graph frames before any effect.
        assert_eq!(
            call("fix", &no_graph)?.unwrap_err().code(),
            "graph_relay_unavailable"
        );
        Ok(())
    }

    #[test]
    fn lost_child_add_reply_replays_the_durable_receipt_over_the_relay()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::graph::{
            GraphCheckpoint, GraphDocument, patch, relay, template,
        };
        use serde_json::json;
        let document: GraphDocument = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"relay-replay","nodes":[{
                "id":"parent","access":"read_only","prompt":"Work","route":{"alias":"senior"}
            }]
        }))?;
        let checkpoint: GraphCheckpoint = serde_json::from_value(json!({
            "schema_version":1,"graph_id":"relay-replay","plan_digest":"unused",
            "nodes":[{"id":"parent","state":"running","worktree":null,"attempts":[]}]
        }))?;
        // Only serialised checkpoint bytes cross calls, like reopening the host
        // journal after an acknowledgement is lost.
        let stored = Mutex::new(serde_json::to_vec(&checkpoint)?);
        let handler = |arguments: Value, _: &CancelToken, _: Duration| {
            let request: relay::ChildRequest =
                serde_json::from_value(arguments).map_err(|_| FacadeError::StateCorrupt)?;
            let mut bytes = stored.lock().map_err(|_| FacadeError::StateCorrupt)?;
            let mut checkpoint: GraphCheckpoint =
                serde_json::from_slice(&bytes).map_err(|_| FacadeError::StateCorrupt)?;
            let library = template::TemplateLibrary::bundled();
            let limits = Default::default();
            let resolvers = Default::default();
            let context = patch::PatchContext {
                library: &library,
                resolvers: &resolvers,
                limits: &limits,
                scan: &|_| Ok(()),
                journal: None,
            };
            let receipt = relay::admit_child(
                &mut patch::LiveGraph {
                    document: &document,
                    checkpoint: &mut checkpoint,
                },
                "parent",
                &request,
                &context,
            )
            .map_err(|error| ServiceError::Cli {
                code: error.code.into(),
                message: error.message,
                data: Value::Null,
            })?;
            *bytes = serde_json::to_vec(&checkpoint)?;
            Ok(serde_json::to_value(receipt)?)
        };
        let request = Request {
            tool: GRAPH_TOOL.into(),
            arguments: json!({
                "id":"writer","brief":"Fix parser","writer":true,"idempotency_key":"add-1"
            }),
        };
        let first = {
            let (mut client, mut server) = UnixStream::pair()?;
            thread::scope(|scope| -> Result<Value, Box<dyn std::error::Error>> {
                let serving = scope.spawn(|| {
                    serve_one_with(
                        &mut server,
                        provenant_records::Limits::CONFIG,
                        &|_, _, _| Err(FacadeError::StateCorrupt.into()),
                        &handler,
                        &CancelToken::new(),
                    )
                });
                let receipt = exchange(&mut client, &request, &CancelToken::new())?;
                serving.join().unwrap()?;
                Ok(receipt)
            })?
        };
        // The lane did not retain the first acknowledgement and retries the add.
        let (mut client, mut server) = UnixStream::pair()?;
        let replay = thread::scope(|scope| -> Result<Value, Box<dyn std::error::Error>> {
            let serving = scope.spawn(|| {
                serve_one_with(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| Err(FacadeError::StateCorrupt.into()),
                    &handler,
                    &CancelToken::new(),
                )
            });
            let receipt = exchange(&mut client, &request, &CancelToken::new())?;
            serving.join().unwrap()?;
            Ok(receipt)
        })?;
        assert_eq!(first["replayed"], false);
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["revision"], first["revision"]);
        assert_eq!(replay["worktree"], ".worktrees/relay-replay-parent--writer");
        let checkpoint: GraphCheckpoint = serde_json::from_slice(&stored.into_inner().unwrap())?;
        assert_eq!(
            checkpoint
                .nodes
                .iter()
                .filter(|node| node.id == "parent--writer")
                .count(),
            1
        );
        assert_eq!(checkpoint.expansions.len(), 1);
        Ok(())
    }

    #[test]
    fn relay_rejects_invalid_graph_input_and_retains_typed_io_facts()
    -> Result<(), Box<dyn std::error::Error>> {
        let invalid = Request {
            tool: "fabric_run".into(),
            arguments: serde_json::json!({"op":"run", "brief":{"TOKEN_SENTINEL":false}}),
        };
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(move || {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| panic!("invalid input must not reach the handler"),
                    &CancelToken::new(),
                )
            });
            let error = exchange(&mut client, &invalid, &CancelToken::new()).unwrap_err();
            let value = error.structured_error();
            assert_eq!(value["code"], "invalid_input");
            assert!(!value.to_string().contains("TOKEN_SENTINEL"));
            serving.join().unwrap().unwrap();
        });
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(move || {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| {
                        Err(FacadeError::Io {
                            operation: "TOKEN_SENTINEL".into(),
                            path: Some("/private/host".into()),
                            delivery_limit: None,
                            delivery_stage: None,
                            resource: None,
                            input_failure: Some(
                                provenant_records::diagnostic::IoFact::InputUnreadable,
                            ),
                        }
                        .into())
                    },
                    &CancelToken::new(),
                )
            });
            let error = exchange(&mut client, &request(), &CancelToken::new()).unwrap_err();
            let value = error.structured_error();
            assert_eq!(value["code"], "io");
            assert!(
                value["detail"]
                    .as_str()
                    .unwrap()
                    .contains("input could not be read")
            );
            assert!(!value.to_string().contains("TOKEN_SENTINEL"));
            assert!(!value.to_string().contains("/private/host"));
            serving.join().unwrap().unwrap();
        });
        Ok(())
    }

    #[test]
    fn relay_host_delivery_limit_keeps_io_detail() -> Result<(), Box<dyn std::error::Error>> {
        let fact = provenant_records::diagnostic::DeliveryLimit::new(
            provenant_records::run::WriterDeliveryStage::Prepare,
            "git",
            "worktree list",
            8388608,
        )
        .unwrap();
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(move || {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| Err(FacadeError::HostDeliveryLimit { fact: fact.clone() }.into()),
                    &CancelToken::new(),
                )
            });
            let error = exchange(&mut client, &request(), &CancelToken::new()).unwrap_err();
            let value = error.structured_error();
            assert_eq!(value["code"], "io");
            let detail = value["detail"].as_str().unwrap();
            assert!(detail.contains("git worktree list"));
            assert!(detail.contains("8388608"));
            serving.join().unwrap().unwrap();
        });
        Ok(())
    }

    #[test]
    fn relay_uncertain_mutation_keeps_operation_identity() -> Result<(), Box<dyn std::error::Error>>
    {
        let operation_id = provenant_core::OperationId::parse("dispatch-17")?;
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(move || {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| {
                        Err(ServiceError::Mutation {
                            operation_id: operation_id.clone(),
                            source: Box::new(ServiceError::Facade(FacadeError::LaunchUnknown {
                                operation_id: operation_id.clone(),
                            })),
                        })
                    },
                    &CancelToken::new(),
                )
            });
            let error = exchange(&mut client, &request(), &CancelToken::new()).unwrap_err();
            let value = error.structured_error();
            assert_eq!(value["code"], "LAUNCH_UNKNOWN");
            assert_eq!(value["operation_id"], "dispatch-17");
            assert!(value["next"].as_str().unwrap().contains("dispatch-17"));
            serving.join().unwrap().unwrap();
        });
        Ok(())
    }

    #[test]
    fn relay_coordination_unknown_keeps_typed_operation_and_remedy()
    -> Result<(), Box<dyn std::error::Error>> {
        let operation_id = provenant_core::OperationId::parse("peer-dispatch-17")?;
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(|| {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| {
                        Err(
                            provenant_fabric_runtime::coordination::CoordinationError::Peer(
                                provenant_fabric_runtime::hosts::PeerError::Unknown {
                                    operation_id: operation_id.clone(),
                                },
                            )
                            .into(),
                        )
                    },
                    &CancelToken::new(),
                )
            });
            let error = exchange(&mut client, &request(), &CancelToken::new()).unwrap_err();
            assert!(matches!(
                &error,
                ServiceError::Facade(FacadeError::LaunchUnknown { operation_id: actual })
                    if actual == &operation_id
            ));
            let value = error.structured_error();
            assert_eq!(value["code"], "LAUNCH_UNKNOWN");
            assert_eq!(value["operation_id"], operation_id.as_str());
            assert!(
                value["next"]
                    .as_str()
                    .unwrap()
                    .contains(operation_id.as_str())
            );
            serving.join().unwrap().unwrap();
        });
        Ok(())
    }

    #[test]
    fn oversized_response_is_reported() -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, mut server) = UnixStream::pair()?;
        thread::scope(|scope| {
            let serving = scope.spawn(|| {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| Ok(Value::String("x".repeat(MAX_BYTES as usize))),
                    &CancelToken::new(),
                )
            });
            assert!(matches!(
                exchange(&mut client, &request(), &CancelToken::new()),
                Err(ServiceError::ResultTooLarge)
            ));
            serving.join().unwrap().unwrap();
        });
        Ok(())
    }

    #[test]
    fn stale_directory_recovery_checks_permissions_and_symlinks() -> io::Result<()> {
        let temp = tempfile::Builder::new().prefix("pf-").tempdir_in(".")?;
        let parent = temp
            .path()
            .strip_prefix(std::env::current_dir()?)
            .map_err(io::Error::other)?
            .join("relay");
        std::fs::DirBuilder::new().mode(0o700).create(&parent)?;
        let path = parent.join("socket");
        drop(UnixListener::bind(&path)?);
        drop(HostRelay::start(
            path.clone(),
            provenant_records::Limits::CONFIG,
            |_, _, _| Ok(Value::Null),
        )?);
        std::fs::DirBuilder::new().mode(0o755).create(&parent)?;
        assert!(
            HostRelay::start(
                path.clone(),
                provenant_records::Limits::CONFIG,
                |_, _, _| Ok(Value::Null)
            )
            .is_err()
        );
        std::fs::remove_dir(&parent)?;
        std::os::unix::fs::symlink(temp.path(), &parent)?;
        assert!(
            HostRelay::start(path, provenant_records::Limits::CONFIG, |_, _, _| Ok(
                Value::Null
            ))
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn caller_budget_reaches_host_without_a_fresh_transport_allowance()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut client, mut server) = UnixStream::pair()?;
        let cancel = CancelToken::new();
        thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
            let serving = scope.spawn(|| {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, within| {
                        assert!(within <= Duration::from_millis(500));
                        assert!(!within.is_zero());
                        Ok(Value::Null)
                    },
                    &cancel,
                )
            });
            assert_eq!(
                exchange_within(&mut client, &request(), &cancel, Duration::from_millis(500))?,
                Value::Null
            );
            serving.join().unwrap()?;
            Ok(())
        })
    }

    #[test]
    fn waiting_observation_does_not_block_another_worker() -> Result<(), Box<dyn std::error::Error>>
    {
        let (mut waiting, waiting_server) = UnixStream::pair()?;
        let (mut mail, mail_server) = UnixStream::pair()?;
        let (started, wait_started) = mpsc::sync_channel(1);
        let (release, wait_release) = mpsc::sync_channel(1);
        let wait_release = Mutex::new(wait_release);
        let handler = |call: ToolCall, _: &CancelToken, _: Duration| {
            if matches!(call, ToolCall::Events(..)) {
                started.send(()).map_err(|_| FacadeError::StateCorrupt)?;
                wait_release
                    .lock()
                    .map_err(|_| FacadeError::StateCorrupt)?
                    .recv_timeout(IO_TIMEOUT)
                    .map_err(|_| FacadeError::Timeout)?;
            } else {
                release.send(()).map_err(|_| FacadeError::StateCorrupt)?;
            }
            Ok(Value::Null)
        };
        let cancel = CancelToken::new();
        let mut results = None;
        serve_connections(
            provenant_records::Limits::CONFIG,
            &handler,
            &no_graph,
            &cancel,
            |send| {
                thread::scope(|scope| {
                    send.send(waiting_server).unwrap();
                    let waiting_call = scope.spawn(|| exchange(&mut waiting, &request(), &cancel));
                    wait_started.recv_timeout(IO_TIMEOUT).unwrap();
                    send.send(mail_server).unwrap();
                    let mail_request = Request {
                        tool: "fabric_mail".into(),
                        arguments: serde_json::json!({"op":"read","after":0,"limit":1}),
                    };
                    let mail_result = exchange(&mut mail, &mail_request, &cancel);
                    results = Some((waiting_call.join().unwrap(), mail_result));
                });
            },
        );
        let (waiting, mail) = results.ok_or("missing replies")?;
        assert!(waiting.is_ok(), "waiting={waiting:?}, mail={mail:?}");
        assert_eq!(waiting?, Value::Null);
        assert_eq!(mail?, Value::Null);
        Ok(())
    }

    #[test]
    fn framing_honours_absolute_deadlines_and_cancellation() -> io::Result<()> {
        let (_client, mut server) = UnixStream::pair()?;
        let mut byte = [0];
        assert_eq!(
            read_bytes(&mut server, &mut byte, Instant::now(), &CancelToken::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let cancel = CancelToken::new();
        cancel.cancel();
        assert_eq!(
            read_bytes(&mut server, &mut byte, Instant::now() + IO_TIMEOUT, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        Ok(())
    }

    #[test]
    fn configured_frame_limits_reject_before_any_effect() -> io::Result<()> {
        let calls = AtomicUsize::new(0);
        for limits in [
            provenant_records::Limits {
                max_bytes: 8,
                ..provenant_records::Limits::CONFIG
            },
            provenant_records::Limits {
                max_depth: 1,
                ..provenant_records::Limits::CONFIG
            },
        ] {
            let (mut client, mut server) = UnixStream::pair()?;
            write(
                &mut client,
                &request(),
                Instant::now() + IO_TIMEOUT,
                &CancelToken::new(),
            )?;
            let result = serve_one(
                &mut server,
                limits,
                &|_, _, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Value::Null)
                },
                &CancelToken::new(),
            );
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn disconnected_queued_caller_cannot_commit_after_admission() -> io::Result<()> {
        let (mut client, mut server) = UnixStream::pair()?;
        let calls = AtomicUsize::new(0);
        thread::scope(|scope| -> io::Result<()> {
            let serving = scope.spawn(|| {
                serve_one(
                    &mut server,
                    provenant_records::Limits::CONFIG,
                    &|_, _, _| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(Value::Null)
                    },
                    &CancelToken::new(),
                )
            });
            write(
                &mut client,
                &request(),
                Instant::now() + IO_TIMEOUT,
                &CancelToken::new(),
            )?;
            let reply: Reply = serde_json::from_slice(&read(
                &mut client,
                MAX_BYTES,
                Instant::now() + IO_TIMEOUT,
                &CancelToken::new(),
            )?)?;
            assert!(matches!(reply, Reply::Ready));
            drop(client);
            assert_eq!(
                serving.join().unwrap().unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            Ok(())
        })?;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
