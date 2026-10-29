//! Attempt-bound host coordination relay. Sender authority stays in the host.
#![allow(clippy::disallowed_methods)] // Native socket I/O uses real deadlines.
use crate::tools::{FacadeError, ServiceError, ToolCall};
use provenant_core::{CancelToken, Clock, DurationMicros, WaitBudget};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, PermissionsExt},
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
    Error { code: String, detail: Value },
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
        // Never reuse or remove an existing entry belonging to another owner.
        let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        std::fs::DirBuilder::new().mode(0o700).create(parent)?;
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
                serve_connections(limits, &handler, &cancel, |send| {
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
                            let _ = serve_one(&mut stream, limits, handler, cancel);
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

pub(crate) fn serve_one(
    stream: &mut UnixStream,
    limits: provenant_records::Limits,
    handler: &impl Fn(ToolCall, &CancelToken, Duration) -> Result<Value, ServiceError>,
    cancel: &CancelToken,
) -> io::Result<()> {
    stream.set_nonblocking(true)?;
    let deadline = Instant::now() + IO_TIMEOUT;
    let bytes = read(stream, limits.max_bytes, deadline, cancel)?;
    let request: Request = provenant_records::decode(bytes.as_slice(), limits)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let reply = match ToolCall::parse(&request.tool, request.arguments) {
        Ok(call) => {
            write(stream, &Reply::Ready, deadline, cancel)?;
            // A queued caller must still be present and confirm its remaining
            // budget before any effect. A timed-out queued call cannot execute.
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
            match handler(call, cancel, Duration::from_millis(milliseconds)) {
                Ok(value) => Reply::Ok(value),
                Err(error) => Reply::Error {
                    code: error.code().into(),
                    detail: error.structured_error(),
                },
            }
        }
        Err(_) => Reply::Error {
            code: "invalid_input".into(),
            detail: Value::Null,
        },
    };
    write(stream, &reply, Instant::now() + IO_TIMEOUT, cancel)
}

pub(crate) fn mutate(
    path: &Path,
    request: &Request,
    cancel: &CancelToken,
    within: Duration,
) -> Result<Value, ServiceError> {
    let mut stream = UnixStream::connect(path).map_err(|_| FacadeError::OwnerUnavailable)?;
    exchange_within(&mut stream, request, cancel, within)
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
            ServiceError::Facade(FacadeError::OwnerUnavailable)
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
        Reply::Error { code, detail } => Err(ServiceError::Cli {
            code,
            message: "host Fabric coordination refused".into(),
            data: detail,
        }),
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
