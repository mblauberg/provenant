//! Bounded newline framing and a four-worker synchronous MCP transport.
//! F9 supplies deadline-aware stdio handles; blocking `Write` is deliberately
//! not accepted because it cannot guarantee EOF shutdown under backpressure.
use crate::protocol::{self, Action, Id, MAX_FRAME, Protocol};
use crate::tools::{FabricService, ServiceError, ToolCall};
use provenant_core::CancelToken;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc, Mutex,
        mpsc::{self, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

const WORKERS: usize = 4;
const QUEUE: usize = 32;
const POLL: Duration = Duration::from_millis(10);
const SHUTDOWN: Duration = Duration::from_secs(56);

/// An OS boundary must return by `deadline`, using WouldBlock/TimedOut when
/// no bytes arrive. Zero means EOF. Only the transport's reader calls this.
pub trait Input {
    fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize>;
}
/// Write one complete frame, including its newline, or fail by `deadline`.
/// Only the transport's writer calls this; diagnostics never use this handle.
pub trait Output {
    fn write_frame(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()>;
}
/// Open each worker's service on that worker, so SQLite never crosses threads.
pub trait Factory: Send + Sync + 'static {
    type Service<'a>: FabricService
    where
        Self: 'a;
    fn open(&self) -> Result<Self::Service<'_>, ServiceError>;
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MCP transport I/O failed")]
    Io(#[from] io::Error),
    #[error("MCP request exceeds the frame limit")]
    Oversize,
    #[error("MCP worker shutdown deadline exceeded")]
    Shutdown,
    #[error("MCP worker failed")]
    Worker,
    #[error("MCP result cannot be encoded")]
    Encode(#[from] serde_json::Error),
}
#[derive(Default)]
pub struct Framer {
    bytes: Vec<u8>,
}
impl Framer {
    pub fn remaining(&self) -> usize {
        MAX_FRAME + 1 - self.bytes.len()
    }
    pub fn push(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > self.remaining() {
            return Err(Error::Oversize);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, Error> {
        if let Some(end) = self.bytes.iter().position(|byte| *byte == b'\n') {
            if end > MAX_FRAME {
                return Err(Error::Oversize);
            }
            let mut frame: Vec<_> = self.bytes.drain(..=end).collect();
            frame.pop();
            return Ok(Some(frame));
        }
        if self.bytes.len() > MAX_FRAME {
            return Err(Error::Oversize);
        }
        Ok(None)
    }
    pub fn incomplete(&self) -> bool {
        !self.bytes.is_empty()
    }
}
struct Job {
    id: Id,
    modern: bool,
    call: ToolCall,
    cancel: CancelToken,
}
struct Completion {
    id: Id,
    reply: Value,
}
fn tool_error(code: &str) -> Value {
    json!({"content":[{"type":"text","text":code}],"structuredContent":{"error":{"code":code}},"isError":true})
}
fn service_error(error: &ServiceError) -> Value {
    let mut value = tool_error(error.code());
    value["structuredContent"] = error.structured_error();
    value
}
fn write(output: &mut impl Output, value: &Value) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        let id = value
            .get("id")
            .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok());
        bytes = serde_json::to_vec(&protocol::error(
            id.as_ref(),
            -32603,
            "Result exceeds frame limit",
        ))?;
    }
    bytes.push(b'\n');
    output.write_frame(&bytes, Instant::now() + Duration::from_millis(250))?;
    Ok(())
}
/// Serve until EOF or an I/O failure. Cancellation affects observations only;
/// launching and stopping runs remain explicit domain operations.
pub fn serve(
    input: &mut impl Input,
    output: &mut impl Output,
    factory: impl Factory,
) -> Result<(), Error> {
    let factory = Arc::new(factory);
    let (jobs, receive) = mpsc::sync_channel::<Job>(QUEUE);
    let receive = Arc::new(Mutex::new(receive));
    let (results, completed) = mpsc::sync_channel::<Completion>(QUEUE + WORKERS);
    let mut workers = Vec::new();
    for _ in 0..WORKERS {
        let (factory, receive, results) = (factory.clone(), receive.clone(), results.clone());
        workers.push(thread::spawn(move || {
            let mut service = factory.open();
            loop {
                let job = match receive.lock() {
                    Ok(reader) => reader.recv(),
                    Err(_) => return,
                };
                let Ok(job) = job else { return };
                let response = if job.cancel.is_cancelled() {
                    tool_error("cancelled")
                } else {
                    match service.as_mut() {
                        Ok(service) => match job.call.execute(service, &job.cancel) {
                            Ok(response) => match serde_json::to_value(response) {
                                Ok(value) => value,
                                Err(_) => tool_error("state_corrupt"),
                            },
                            Err(error) => service_error(&error),
                        },
                        Err(error) => service_error(error),
                    }
                };
                if results
                    .try_send(Completion {
                        id: job.id.clone(),
                        reply: protocol::result(&job.id, response, job.modern),
                    })
                    .is_err()
                {
                    return;
                }
            }
        }));
    }
    drop(results);
    let mut live = BTreeMap::<Id, CancelToken>::new();
    let mut protocol = Protocol::default();
    let mut framer = Framer::default();
    let mut buffer = [0u8; 8192];
    let run = (|| {
        loop {
            while let Ok(done) = completed.try_recv() {
                if let Some(cancel) = live.remove(&done.id)
                    && !cancel.is_cancelled()
                {
                    write(output, &done.reply)?;
                }
            }
            if let Some(frame) = framer.next_frame()? {
                match protocol.receive(&frame) {
                    Action::Ignore => {}
                    Action::Cancel(id) => {
                        if let Some(cancel) = live.get(&id) {
                            cancel.cancel();
                        }
                    }
                    Action::Reply(reply) => {
                        // Protocol controls cannot reuse a running request ID either.
                        let id = reply
                            .get("id")
                            .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok());
                        if id.as_ref().is_some_and(|id| live.contains_key(id)) {
                            write(
                                output,
                                &protocol::error(id.as_ref(), -32600, "Duplicate live request ID"),
                            )?;
                        } else {
                            write(output, &reply)?;
                        }
                    }
                    Action::Call {
                        id,
                        modern,
                        name,
                        arguments,
                    } => {
                        if live.contains_key(&id) {
                            write(
                                output,
                                &protocol::error(Some(&id), -32600, "Duplicate live request ID"),
                            )?;
                            continue;
                        }
                        let call = match ToolCall::parse(&name, arguments) {
                            Ok(call) => call,
                            Err(_) => {
                                write(
                                    output,
                                    &protocol::error(
                                        Some(&id),
                                        -32602,
                                        "Invalid tool name or arguments",
                                    ),
                                )?;
                                continue;
                            }
                        };
                        let cancel = CancelToken::new();
                        let job = Job {
                            id: id.clone(),
                            modern,
                            call,
                            cancel: cancel.clone(),
                        };
                        match jobs.try_send(job) {
                            Ok(()) => {
                                live.insert(id, cancel);
                            }
                            Err(TrySendError::Full(_)) => {
                                write(output, &protocol::result(&id, tool_error("busy"), modern))?
                            }
                            Err(TrySendError::Disconnected(_)) => return Err(Error::Worker),
                        }
                    }
                }
                continue;
            }
            let space = framer.remaining().min(buffer.len());
            match input.read(&mut buffer[..space], Instant::now() + POLL) {
                Ok(0) => {
                    if framer.incomplete() {
                        write(
                            output,
                            &protocol::error(None, -32700, "EOF inside a JSON frame"),
                        )?;
                    }
                    return Ok(());
                }
                Ok(size) => {
                    let chunk = buffer.get(..size).ok_or(Error::Worker)?;
                    framer.push(chunk)?;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
    })();
    // EOF/error closes admission and cancels only cooperative observations.
    for cancel in live.values() {
        cancel.cancel();
    }
    drop(jobs);
    let deadline = Instant::now() + SHUTDOWN;
    while workers.iter().any(|worker| !worker.is_finished()) {
        if Instant::now() >= deadline {
            return Err(Error::Shutdown);
        }
        // Bounded waits also drain results, allowing workers to retire.
        let _ = completed.recv_timeout(POLL);
    }
    for worker in workers {
        worker.join().map_err(|_| Error::Worker)?;
    }
    run
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
