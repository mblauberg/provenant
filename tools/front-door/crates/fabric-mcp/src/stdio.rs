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
    shutdown: Duration::from_secs(56),
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
enum Event {
    Frame(Vec<u8>),
    InputEof(bool),
    InputError(io::Error),
    Oversize,
    Completed(Completion),
}

fn failure(code: &str) -> Value {
    tools::tool_error(code, json!({"code":code}))
}
fn service_error(error: &ServiceError) -> Value {
    tools::tool_error(error.code(), error.structured_error())
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
            failure("result_too_large"),
            result.get("resultType").is_some(),
        ))?;
    }
    if bytes.len() > MAX_FRAME {
        return Err(Error::OversizeReply);
    }
    bytes.push(b'\n');
    output.write_frame(&bytes, deadline).map_err(Error::Output)
}
fn write(output: &mut impl Output, value: &Value, timeouts: Timeouts) -> Result<(), Error> {
    write_before(output, value, Instant::now() + timeouts.write)
}

struct CompletionGuard {
    id: Id,
    modern: bool,
    events: SyncSender<Event>,
    response: Option<Value>,
}
impl CompletionGuard {
    fn new(id: Id, modern: bool, events: SyncSender<Event>) -> Self {
        Self {
            id,
            modern,
            events,
            response: None,
        }
    }
}
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        let response = self
            .response
            .take()
            .unwrap_or_else(|| failure("worker_panicked"));
        let reply = protocol::result(&self.id, response, self.modern);
        let _ = self.events.send(Event::Completed(Completion {
            id: self.id.clone(),
            reply,
        }));
    }
}

fn read_input(
    input: &mut impl Input,
    events: &SyncSender<Event>,
    stop: &AtomicBool,
    timeouts: Timeouts,
) {
    let mut buffer = [0_u8; 8192];
    let mut framer = Framer::default();
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
    let factory = Arc::new(factory);
    let (jobs, receive) = mpsc::sync_channel::<Job>(QUEUE);
    let receive = Arc::new(Mutex::new(receive));
    let (events_tx, events) = mpsc::sync_channel::<Event>(EVENT_QUEUE);
    let reader_events = events_tx.clone();
    let mut workers = Vec::with_capacity(WORKERS);
    for _ in 0..WORKERS {
        let (factory, receive, events) = (factory.clone(), receive.clone(), events_tx.clone());
        workers.push(thread::spawn(move || {
            let mut service = factory.open();
            loop {
                let received = match receive.lock() {
                    Ok(reader) => reader.recv(),
                    Err(_) => return,
                };
                let Ok(job) = received else { return };
                let mut completion =
                    CompletionGuard::new(job.id.clone(), job.modern, events.clone());
                let response = if job.cancel.is_cancelled() {
                    failure("cancelled")
                } else {
                    match service.as_mut() {
                        Ok(service) => match job.call.execute(service, &job.cancel) {
                            Ok(response) => match serde_json::to_value(response) {
                                Ok(value) => value,
                                Err(_) => failure("state_corrupt"),
                            },
                            Err(error) => service_error(&error),
                        },
                        Err(error) => service_error(error),
                    }
                };
                completion.response = Some(response);
            }
        }));
    }
    drop(events_tx);

    let stop_reader = AtomicBool::new(false);
    let mut live = BTreeMap::<Id, CancelToken>::new();
    let run_result = thread::scope(|scope| {
        let reader = scope.spawn(|| read_input(input, &reader_events, &stop_reader, timeouts));
        let run = serve_loop(&events, &jobs, output, &mut live, &workers, timeouts);
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
        );
        // Input::read must honour its deadline: after stop, at most one
        // input_read interval remains. A reader blocked sending to the bounded
        // event queue is released by this drain; no producer can refill it
        // indefinitely once admission stops and the finite job queue drains.
        // Polling adds at most one event interval before the scoped join.
        while !reader.is_finished() {
            let _ = events.recv_timeout(timeouts.event);
        }
        if reader.join().is_err() {
            return Err(Error::Worker);
        }
        match shutdown {
            Err(error) => Err(error),
            Ok(()) => run,
        }
    });
    for worker in workers {
        if worker.is_finished() {
            let _ = worker.join();
        }
    }
    run_result
}

fn serve_loop(
    events: &Receiver<Event>,
    jobs: &SyncSender<Job>,
    output: &mut impl Output,
    live: &mut BTreeMap<Id, CancelToken>,
    workers: &[thread::JoinHandle<()>],
    timeouts: Timeouts,
) -> Result<(), Error> {
    let mut protocol = Protocol::default();
    loop {
        match events.recv_timeout(timeouts.event) {
            Ok(Event::Frame(frame)) => match protocol.receive(&frame) {
                Action::Ignore => {}
                Action::Cancel(id) => {
                    if let Some(cancel) = live.get(&id) {
                        cancel.cancel();
                    }
                }
                Action::Reply(reply) => {
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
                    if live.contains_key(&id) {
                        write(
                            output,
                            &protocol::error(Some(&id), -32600, "Duplicate live request ID"),
                            timeouts,
                        )?;
                        continue;
                    }
                    let cancel = CancelToken::new();
                    let job = Job {
                        id: id.clone(),
                        modern,
                        call: *call,
                        cancel: cancel.clone(),
                    };
                    match jobs.try_send(job) {
                        Ok(()) => {
                            live.insert(id, cancel);
                        }
                        Err(TrySendError::Full(_)) => write(
                            output,
                            &protocol::result(&id, failure("busy"), modern),
                            timeouts,
                        )?,
                        Err(TrySendError::Disconnected(_)) => return Err(Error::Worker),
                    }
                }
            },
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
                if let Some(cancel) = live.remove(&completion.id)
                    && !cancel.is_cancelled()
                {
                    write(output, &completion.reply, timeouts)?;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Error::Worker),
        }
        if workers.iter().all(thread::JoinHandle::is_finished) {
            return Err(Error::Worker);
        }
    }
}

fn shutdown_workers(
    events: &Receiver<Event>,
    output: &mut impl Output,
    live: &mut BTreeMap<Id, CancelToken>,
    workers: &mut Vec<thread::JoinHandle<()>>,
    cancel_now: bool,
    timeouts: Timeouts,
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
        let finished = workers.iter().all(thread::JoinHandle::is_finished);
        // Once every worker has returned, all completion guards have sent.
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
            Some(Event::Completed(completion)) => {
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
    let panicked = workers
        .drain(..)
        .map(|worker| worker.join().is_err())
        .filter(|failed| *failed)
        .count();
    if !live.is_empty() || panicked == WORKERS {
        return output_error.map_or(Err(Error::Worker), Err);
    }
    output_error.map_or(Ok(()), Err)
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
