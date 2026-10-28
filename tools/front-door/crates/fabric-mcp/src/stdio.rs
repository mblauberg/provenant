//! Bounded newline framing and a four-worker synchronous MCP transport.
use crate::protocol::{self, Action, Id, MAX_FRAME, Protocol};
use crate::tools::{self, F3Facade, Service, ServiceError, ToolCall};
use provenant_core::CancelToken;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

const WORKERS: usize = 4;
const QUEUE: usize = 32;
const EVENT_QUEUE: usize = QUEUE + WORKERS + 8;
#[derive(Clone, Copy)]
struct Timeouts {
    input_read: Duration,
    event: Duration,
    write: Duration,
    shutdown: Duration,
    cancel_grace: Duration,
}
const TIMEOUTS: Timeouts = Timeouts {
    input_read: Duration::from_secs(1),
    event: Duration::from_secs(1),
    // The design bounds synchronous peer calls and stdout backpressure to 30s.
    write: Duration::from_secs(30),
    shutdown: Duration::from_millis(crate::MAX_WAIT_SECONDS * 1_000 + 1_000),
    cancel_grace: Duration::from_secs(1),
};

/// An OS boundary must return by `deadline`, using WouldBlock/TimedOut when
/// no bytes arrive. Zero means EOF. Only the transport's reader calls this.
pub trait Input: Send {
    fn read(&mut self, bytes: &mut [u8], deadline: Instant) -> io::Result<usize>;
}
/// Write one complete frame, including its newline, or fail by `deadline`.
/// Only the transport's writer calls this; diagnostics never use this handle.
pub trait Output {
    fn write_frame(&mut self, bytes: &[u8], deadline: Instant) -> io::Result<()>;
}
/// Open each worker's service on that worker, so SQLite never crosses threads.
pub trait Factory: Send + Sync + 'static {
    type Facade<'a>: F3Facade
    where
        Self: 'a;
    fn open(&self) -> Result<Service<'_, Self::Facade<'_>>, ServiceError>;
    /// Context-only refusals must precede unrelated owner initialisation.
    fn preflight(&self, _call: &tools::ToolCall) -> Result<(), ServiceError> {
        Ok(())
    }
    /// Log maintenance runs only on an existing worker.
    fn maintain_usage(&self) {}
    /// Optional composition hook. Names/outcomes are fixed metadata, never arguments.
    fn record_usage(&self, _tool: &str, _outcome: &str, _duration_ms: u64) {}
    fn input_limits(&self) -> provenant_records::Limits {
        provenant_records::Limits {
            max_bytes: MAX_FRAME as u64,
            max_depth: 64,
            max_members: 100_000,
        }
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MCP transport I/O failed")]
    Io(#[from] io::Error),
    #[error("MCP output I/O failed")]
    Output(io::Error),
    #[error("MCP request exceeds the frame limit")]
    Oversize,
    #[error("MCP response exceeds the frame limit")]
    OversizeReply,
    #[error("MCP worker shutdown deadline exceeded")]
    Shutdown,
    #[error("MCP worker failed")]
    Worker,
    #[error("MCP result cannot be encoded")]
    Encode(#[from] serde_json::Error),
}
pub struct Framer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Default for Framer {
    fn default() -> Self {
        Self::with_limit(MAX_FRAME)
    }
}
impl Framer {
    pub fn with_limit(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit: limit.min(MAX_FRAME),
        }
    }
    pub fn remaining(&self) -> usize {
        self.limit + 1 - self.bytes.len()
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
            if end > self.limit {
                return Err(Error::Oversize);
            }
            let mut frame: Vec<_> = self.bytes.drain(..=end).collect();
            frame.pop();
            return Ok(Some(frame));
        }
        if self.bytes.len() > self.limit {
            return Err(Error::Oversize);
        }
        Ok(None)
    }
    pub fn incomplete(&self) -> bool {
        !self.bytes.is_empty()
    }
}

struct Job {
    tool: &'static str,
    started: Instant,
    id: Id,
    modern: bool,
    call: ToolCall,
    cancel: CancelToken,
}
struct Completion {
    tool: &'static str,
    started: Instant,
    id: Id,
    reply: Value,
}
enum Event {
    Frame(Vec<u8>),
    InputEof(bool),
    InputError(io::Error),
    Oversize,
    Completed(Completion),
    WorkerExited,
}

fn failure(tool: Option<&str>, code: &str) -> Value {
    tools::tool_error(tool, code, json!({"code":code}))
}
fn service_error(tool: &str, error: &ServiceError) -> Value {
    tools::tool_error(Some(tool), error.code(), error.structured_error())
}

fn write_before(output: &mut impl Output, value: &Value, deadline: Instant) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME {
        let Some(id) = value
            .get("id")
            .cloned()
            .and_then(|id| serde_json::from_value::<Id>(id).ok())
        else {
            return Err(Error::OversizeReply);
        };
        let Some(result) = value
            .get("result")
            .filter(|result| result.get("content").is_some())
        else {
            return Err(Error::OversizeReply);
        };
        bytes = serde_json::to_vec(&protocol::result(
            &id,
            failure(None, "result_too_large"),
            result.get("resultType").is_some(),
        ))?;
    }
    if bytes.len() > MAX_FRAME {
        return Err(Error::OversizeReply);
    }
    bytes.push(b'\n');
    output.write_frame(&bytes, deadline).map_err(Error::Output)
}
// I/O deadlines use the operating system's real monotonic coordinate.
#[allow(clippy::disallowed_methods)]
fn write(output: &mut impl Output, value: &Value, timeouts: Timeouts) -> Result<(), Error> {
    write_before(output, value, Instant::now() + timeouts.write)
}

struct WorkerExitGuard(SyncSender<Event>);
impl Drop for WorkerExitGuard {
    fn drop(&mut self) {
        let _ = self.0.send(Event::WorkerExited);
    }
}

struct Workers {
    threads: Vec<thread::JoinHandle<()>>,
    remaining: usize,
}

struct CompletionGuard {
    tool: &'static str,
    started: Instant,
    id: Id,
    modern: bool,
    events: SyncSender<Event>,
    response: Option<Value>,
}
impl CompletionGuard {
    fn new(
        id: Id,
        modern: bool,
        events: SyncSender<Event>,
        tool: &'static str,
        started: Instant,
    ) -> Self {
        Self {
            id,
            modern,
            events,
            response: None,
            tool,
            started,
        }
    }
}
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let response = self
            .response
            .take()
            .unwrap_or_else(|| failure(Some(self.tool), "worker_panicked"));
        let reply = protocol::result(&self.id, response, self.modern);
        let _ = self.events.send(Event::Completed(Completion {
            id: self.id.clone(),
            reply,
            tool: self.tool,
            started: self.started,
        }));
    }
}

// Input poll deadlines are real transport boundaries.
#[allow(clippy::disallowed_methods)]
fn read_input(
    input: &mut impl Input,
    events: &SyncSender<Event>,
    stop: &AtomicBool,
    timeouts: Timeouts,
    limit: usize,
) {
    let mut buffer = [0_u8; 8192];
    let mut framer = Framer::with_limit(limit);
    while !stop.load(Ordering::Acquire) {
        let capacity = framer.remaining().min(buffer.len());
        match input.read(
            &mut buffer[..capacity],
            Instant::now() + timeouts.input_read,
        ) {
            Ok(0) => {
                let _ = events.send(Event::InputEof(framer.incomplete()));
                return;
            }
            Ok(size) => {
                let Some(bytes) = buffer.get(..size) else {
                    let _ = events.send(Event::InputError(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "input returned more bytes than its buffer",
                    )));
                    return;
                };
                if framer.push(bytes).is_err() {
                    let _ = events.send(Event::Oversize);
                    return;
                }
                loop {
                    match framer.next_frame() {
                        Ok(Some(frame)) => {
                            if events.send(Event::Frame(frame)).is_err() {
                                return;
                            }
                        }
                        Err(Error::Oversize) => {
                            let _ = events.send(Event::Oversize);
                            return;
                        }
                        Ok(None) => break,
                        Err(_) => return,
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => {
                let _ = events.send(Event::InputError(error));
                return;
            }
        }
    }
}

/// Serve until EOF or an I/O failure. Cancellation affects observations only;
/// launching and stopping runs remain explicit domain operations.
pub fn serve(
    input: &mut impl Input,
    output: &mut impl Output,
    factory: impl Factory,
) -> Result<(), Error> {
    serve_with_timeouts(input, output, factory, TIMEOUTS)
}

fn serve_with_timeouts(
    input: &mut impl Input,
    output: &mut impl Output,
    factory: impl Factory,
    timeouts: Timeouts,
) -> Result<(), Error> {
    let limits = factory.input_limits();
    let factory = Arc::new(factory);
    let (jobs, receive) = mpsc::sync_channel::<Job>(QUEUE);
    let receive = Arc::new(Mutex::new(receive));
    let (events_tx, events) = mpsc::sync_channel::<Event>(EVENT_QUEUE);
    let reader_events = events_tx.clone();
    let mut workers = Workers {
        threads: Vec::with_capacity(WORKERS),
        remaining: WORKERS,
    };
    for _ in 0..WORKERS {
        let (factory, receive, events) = (factory.clone(), receive.clone(), events_tx.clone());
        workers.threads.push(thread::spawn(move || {
            // Declared first so service cleanup and completion guards run before
            // the exit event, including panics outside request execution.
            let _exit = WorkerExitGuard(events.clone());
            let mut service = None;
            loop {
                let received = match receive.lock() {
                    Ok(reader) => reader.recv(),
                    Err(_) => return,
                };
                let Ok(job) = received else { return };
                factory.maintain_usage();
                let mut completion = CompletionGuard::new(
                    job.id.clone(),
                    job.modern,
                    events.clone(),
                    job.tool,
                    job.started,
                );
                let response = if job.cancel.is_cancelled() {
                    let mut detail = json!({"code":"cancelled"});
                    if let Some(operation_id) = job.call.operation_id() {
                        detail["operation_id"] = json!(operation_id);
                    }
                    tools::tool_error(Some(job.tool), "cancelled", detail)
                } else {
                    if let Err(error) = factory.preflight(&job.call) {
                        let error = match job.call.operation_id() {
                            Some(operation_id) => ServiceError::Mutation {
                                operation_id,
                                source: Box::new(error),
                            },
                            None => error,
                        };
                        completion.response = Some(service_error(job.tool, &error));
                        continue;
                    }
                    if service.is_none() {
                        match factory.open() {
                            Ok(opened) => service = Some(opened),
                            Err(error) => {
                                let error = match job.call.operation_id() {
                                    Some(operation_id) => ServiceError::Mutation {
                                        operation_id,
                                        source: Box::new(error),
                                    },
                                    None => error,
                                };
                                completion.response = Some(service_error(job.tool, &error));
                                continue;
                            }
                        }
                    }
                    match service.as_mut() {
                        Some(service) => match job.call.execute(service, &job.cancel) {
                            Ok(response) => match serde_json::to_value(response) {
                                Ok(value) => value,
                                Err(_) => failure(Some(job.tool), "state_corrupt"),
                            },
                            Err(error) => service_error(job.tool, &error),
                        },
                        None => failure(Some(job.tool), "state_corrupt"),
                    }
                };
                completion.response = Some(response);
            }
        }));
    }
    drop(events_tx);

    let stop_reader = AtomicBool::new(false);
    let mut live = BTreeMap::<Id, CancelToken>::new();
    thread::scope(|scope| {
        let reader = scope.spawn(|| {
            read_input(
                input,
                &reader_events,
                &stop_reader,
                timeouts,
                usize::try_from(limits.max_bytes).unwrap_or(MAX_FRAME),
            )
        });
        let run = serve_loop(
            &events,
            &jobs,
            output,
            &mut live,
            &mut workers.remaining,
            timeouts,
            factory.as_ref(),
        );
        stop_reader.store(true, Ordering::Release);
        drop(jobs);
        let output_failed = matches!(&run, Err(Error::Output(_) | Error::OversizeReply));
        let shutdown = shutdown_workers(
            &events,
            output,
            &mut live,
            &mut workers,
            output_failed || matches!(&run, Err(Error::Worker)),
            timeouts,
            factory.as_ref(),
        );
        // Input::read must honour its deadline: after stop, at most one
        // input_read interval remains. A reader blocked sending to the bounded
        // event queue is released by this drain; no producer can refill it
        // indefinitely once admission stops and the finite job queue drains.
        // Poll no slower than the input deadline when only the reader remains.
        while !reader.is_finished() {
            let _ = events.recv_timeout(timeouts.event.min(timeouts.input_read));
        }
        // Draining has stopped; disconnect late guard sends before any joins.
        drop(events);
        if reader.join().is_err() {
            return Err(Error::Worker);
        }
        let drained = workers.remaining == 0;
        let panicked = workers
            .threads
            .drain(..)
            .filter(|worker| drained || worker.is_finished())
            .map(|worker| worker.join().is_err())
            .filter(|failed| *failed)
            .count();
        match shutdown {
            Err(error) => Err(error),
            Ok(()) if panicked == WORKERS => Err(Error::Worker),
            Ok(()) => run,
        }
    })
}

// The transport loop bounds shutdown and output using real elapsed time.
#[allow(clippy::disallowed_methods)]
fn serve_loop(
    events: &Receiver<Event>,
    jobs: &SyncSender<Job>,
    output: &mut impl Output,
    live: &mut BTreeMap<Id, CancelToken>,
    workers_remaining: &mut usize,
    timeouts: Timeouts,
    factory: &impl Factory,
) -> Result<(), Error> {
    let limits = factory.input_limits();
    let mut protocol = Protocol::default();
    loop {
        match events.recv_timeout(timeouts.event) {
            Ok(Event::Frame(frame)) => {
                let started = Instant::now();
                let action = protocol.receive_with_limits(&frame, limits);
                let tool = protocol.usage_tool();
                match action {
                    Action::Ignore => {}
                    Action::Cancel(id) => {
                        if let Some(cancel) = live.get(&id) {
                            cancel.cancel();
                        }
                    }
                    Action::Reply(reply) => {
                        if let Some(tool) = tool {
                            factory.record_usage(tool, "error", 0);
                        }
                        let id = reply
                            .get("id")
                            .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok());
                        if id.as_ref().is_some_and(|id| live.contains_key(id)) {
                            write(
                                output,
                                &protocol::error(id.as_ref(), -32600, "Duplicate live request ID"),
                                timeouts,
                            )?;
                        } else {
                            write(output, &reply, timeouts)?;
                        }
                    }
                    Action::Call { id, modern, call } => {
                        let tool = call.name();
                        if live.contains_key(&id) {
                            factory.record_usage(tool, "error", 0);
                            write(
                                output,
                                &protocol::error(Some(&id), -32600, "Duplicate live request ID"),
                                timeouts,
                            )?;
                            continue;
                        }
                        let cancel = CancelToken::new();
                        let job = Job {
                            tool,
                            started,
                            id: id.clone(),
                            modern,
                            call: *call,
                            cancel: cancel.clone(),
                        };
                        match jobs.try_send(job) {
                            Ok(()) => {
                                live.insert(id, cancel);
                            }
                            Err(TrySendError::Full(job)) => {
                                factory.record_usage(job.tool, "busy", 0);
                                write(
                                    output,
                                    &protocol::result(&id, failure(Some(job.tool), "busy"), modern),
                                    timeouts,
                                )?;
                            }
                            Err(TrySendError::Disconnected(job)) => {
                                factory.record_usage(job.tool, "error", 0);
                                return Err(Error::Worker);
                            }
                        }
                    }
                }
            }
            Ok(Event::Oversize) => {
                write(
                    output,
                    &protocol::uncorrelated_error(
                        -32700,
                        "Request exceeds the frame limit",
                        protocol.modern(),
                    ),
                    timeouts,
                )?;
                return Err(Error::Oversize);
            }
            Ok(Event::InputEof(incomplete)) => {
                if incomplete {
                    write(
                        output,
                        &protocol::uncorrelated_error(
                            -32700,
                            "EOF inside a JSON frame",
                            protocol.modern(),
                        ),
                        timeouts,
                    )?;
                }
                return Ok(());
            }
            Ok(Event::InputError(error)) => return Err(Error::Io(error)),
            Ok(Event::Completed(completion)) => {
                record_completion(factory, &completion);
                if let Some(cancel) = live.remove(&completion.id)
                    && !cancel.is_cancelled()
                {
                    write(output, &completion.reply, timeouts)?;
                }
            }
            Ok(Event::WorkerExited) => {
                *workers_remaining -= 1;
                if *workers_remaining == 0 {
                    return Err(Error::Worker);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Error::Worker),
        }
    }
}

fn record_completion(factory: &impl Factory, completion: &Completion) {
    let outcome = if completion.reply["result"]["isError"] == true {
        "error"
    } else {
        "ok"
    };
    factory.record_usage(
        completion.tool,
        outcome,
        u64::try_from(completion.started.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
}

// Worker shutdown must remain bounded even when no further input arrives.
#[allow(clippy::disallowed_methods)]
fn shutdown_workers(
    events: &Receiver<Event>,
    output: &mut impl Output,
    live: &mut BTreeMap<Id, CancelToken>,
    workers: &mut Workers,
    cancel_now: bool,
    timeouts: Timeouts,
    factory: &impl Factory,
) -> Result<(), Error> {
    let deadline = Instant::now() + timeouts.shutdown;
    let mut cancelled = cancel_now;
    let mut output_error = None;
    loop {
        let now = Instant::now();
        cancelled |= now >= deadline - timeouts.cancel_grace;
        if cancelled {
            for cancel in live.values() {
                cancel.cancel();
            }
        }
        if now >= deadline {
            return Err(Error::Shutdown);
        }
        let finished = workers.remaining == 0;
        // Once every worker has reported its exit, all completion guards have sent.
        // Drain those queued completions without waiting for another sender.
        let wake = if cancelled {
            deadline
        } else {
            deadline - timeouts.cancel_grace
        };
        let event = if finished {
            events.try_recv().ok()
        } else {
            events
                .recv_timeout(timeouts.event.min(wake.saturating_duration_since(now)))
                .ok()
        };
        match event {
            Some(Event::WorkerExited) => workers.remaining -= 1,
            Some(Event::Completed(completion)) => {
                record_completion(factory, &completion);
                if let Some(cancel) = live.remove(&completion.id)
                    && !cancel.is_cancelled()
                    && output_error.is_none()
                    && let Err(error) = write_before(
                        output,
                        &completion.reply,
                        (Instant::now() + timeouts.write).min(deadline),
                    )
                {
                    output_error = Some(error);
                    cancelled = true;
                }
            }
            None if finished => break,
            _ => {}
        }
    }
    if !live.is_empty() {
        return output_error.map_or(Err(Error::Worker), Err);
    }
    output_error.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
