//! `provenant wait`: block until graph targets or pull requests settle. A wait
//! is a reader: neither expiry nor cancellation stops the work. Local graphs
//! share one event source; pull requests remain on forge observation.
// This edge owns the real deadline for channel receives and source I/O.
#![allow(clippy::disallowed_methods)]
use provenant_cli::Globals;
use provenant_core::CancelToken;
use provenant_records::Outcome;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
enum Source {
    Graph(provenant_cli::Target),
    UnreadableGraph(provenant_cli::Target, String),
    Pr {
        number: u64,
        repository: Result<String, String>,
    },
}
impl Source {
    fn label(&self) -> String {
        match self {
            Self::Graph(target) | Self::UnreadableGraph(target, _) => target.to_string(),
            Self::Pr { number, .. } => format!("#{number}"),
        }
    }
}

/// Where one target stands for a waiter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Settle {
    Pending,
    NeedsInput,
    Succeeded,
    Unsuccessful,
}

#[derive(Clone, Debug, PartialEq)]
struct Reading {
    settle: Settle,
    /// `TARGET STATE [cause]`, bounded; equal lines are one observation.
    line: String,
    data: Value,
}
impl Reading {
    fn done(&self) -> bool {
        self.settle != Settle::Pending
    }
    fn unavailable(label: &str, error: &str) -> Self {
        Self {
            settle: Settle::Pending,
            line: provenant_cli::bounded_text(
                &format!("{label} unavailable: {error}"),
                provenant_cli::limits::SUMMARY_ROW_BYTES,
            ),
            data: Value::Null,
        }
    }
}

/// A graph target settles by its own row: the graph, else its node, else its
/// newest attempt. A complete graph or a settled attempt succeeds only with a
/// succeeded outcome; a paused graph needs input.
fn classify_rows(label: &str, rows: &[Value]) -> Reading {
    let pick = |kind: &str| rows.iter().rfind(|row| row["kind"] == kind);
    let data = json!({ "rows": rows, "digests": crate::fabric::node_digests(rows) });
    let Some(row) = pick("graph")
        .or_else(|| pick("node"))
        .or_else(|| pick("attempt"))
    else {
        return Reading {
            settle: Settle::Pending,
            line: format!("{label} pending"),
            data,
        };
    };
    let state = row["state"].as_str().unwrap_or("");
    let succeeded = row["outcome"] == "succeeded" || state == "succeeded";
    let settle = match row["kind"].as_str() {
        Some("graph") => match state {
            "complete" if succeeded => Settle::Succeeded,
            "complete" | "cancelled" | "unreadable" => Settle::Unsuccessful,
            "paused" => Settle::NeedsInput,
            _ => Settle::Pending,
        },
        Some("node") => match state {
            "succeeded" => Settle::Succeeded,
            "failed" | "blocked" | "skipped" => Settle::Unsuccessful,
            _ => Settle::Pending,
        },
        _ if state == "terminal" || !row["finished_at"].is_null() => {
            if succeeded {
                Settle::Succeeded
            } else {
                Settle::Unsuccessful
            }
        }
        _ if state == "unreadable" => Settle::Unsuccessful,
        _ => Settle::Pending,
    };
    Reading {
        settle,
        line: crate::fabric::status_line(label, row, None)
            .trim_end()
            .to_owned(),
        data,
    }
}

/// Read one source after a native event or a configured fallback tick.
fn read(
    source: &Source,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<provenant_cli::fabric::Context>,
    deadline: Instant,
    pr_read_timeout: Duration,
    cancel: &CancelToken,
) -> Reading {
    let label = source.label();
    let left = deadline.saturating_duration_since(Instant::now());
    if cancel.is_cancelled() || left.is_zero() {
        return Reading::unavailable(&label, "wait deadline or cancellation reached");
    }
    let reading = match source {
        Source::UnreadableGraph(_, error) => classify_rows(
            &label,
            &[json!({
                "kind":"graph", "state":"unreadable", "reason":error,
            })],
        ),
        Source::Graph(target) => {
            match provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
                crate::fabric::read_status_rows(target, edge, session, deadline, cancel)
            }) {
                Ok(rows) => {
                    return classify_rows(&label, &rows);
                }
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            }
        }
        Source::Pr { number, repository } => match repository {
            Ok(repository) => match crate::pr::status::wait_line_observed(
                edge.cwd,
                edge.roots.state.as_path(),
                repository,
                *number,
                left.min(pr_read_timeout),
                crate::pr::observer::POLL,
            ) {
                Ok(line) => Reading {
                    settle: match line.verdict() {
                        Some(true) => Settle::Succeeded,
                        Some(false) => Settle::Unsuccessful,
                        None => Settle::Pending,
                    },
                    line: format!("{label} {}", line.class()),
                    data: json!(line),
                },
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            },
            Err(error) => Reading::unavailable(&label, error),
        },
    };
    reading
}

/// Cancellation wakes source pauses and interrupts in-flight GitHub reads.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
    cancel: CancelToken,
    event_wake: Mutex<Option<provenant_os::changes::watch::WakeHandle>>,
}
impl Stop {
    fn install_event_wake(&self, wake: provenant_os::changes::watch::WakeHandle) {
        if let Ok(mut slot) = self.event_wake.lock() {
            *slot = Some(wake.clone());
        }
        if self.cancel.is_cancelled() {
            let _ = wake.wake();
        }
    }
    fn stop(&self) {
        self.cancel.cancel();
        if let Ok(slot) = self.event_wake.lock() {
            if let Some(wake) = slot.as_ref() {
                let _ = wake.wake();
            }
        }
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
        }
        self.wake.notify_all();
    }
    fn pause(&self, duration: Duration) -> bool {
        let Ok(stopped) = self.stopped.lock() else {
            return false;
        };
        self.wake
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .is_ok_and(|(stopped, _)| !*stopped)
    }
}

struct Update {
    target: usize,
    reading: Reading,
}

pub(crate) fn graph_event_path(
    state: &std::path::Path,
    project: provenant_core::ProjectId,
    graph: &str,
) -> PathBuf {
    state
        .join("fabric")
        .join("graphs")
        .join(project.to_string())
        .join(format!("{graph}.events.jsonl"))
}

pub(crate) fn observation_text(
    watcher: Option<&provenant_os::changes::watch::Watcher>,
    interval: Duration,
    fallback: Option<&str>,
) -> String {
    match watcher {
        Some(watcher) => format!(
            "filesystem events via {}",
            match watcher.backend() {
                provenant_os::changes::watch::Backend::Inotify => "inotify",
                provenant_os::changes::watch::Backend::Kqueue => "kqueue",
                provenant_os::changes::watch::Backend::Unsupported => "unsupported",
            }
        ),
        None => format!(
            "paced reread every {} ms (domains.wait.local_interval_ms); event source unavailable: {}",
            interval.as_millis(),
            fallback.unwrap_or("no event source")
        ),
    }
}

fn rows_local<H: std::str::FromStr + PartialEq>(rows: &[Value], host: Option<&H>) -> bool {
    host.is_some_and(|host| {
        rows.iter().all(|row| {
            row["kind"] != "attempt"
                || row["attempt_id"]
                    .as_str()
                    .and_then(|id| id.split_once('/'))
                    .is_none_or(|(owner, _)| owner.parse::<H>().is_ok_and(|owner| &owner == host))
        })
    })
}

fn reading_locally_represented(reading: &Reading, host: Option<&provenant_core::HostId>) -> bool {
    reading.data["rows"]
        .as_array()
        .is_some_and(|rows| rows_local(rows, host))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GraphWake {
    Event,
    NativeTimeout,
    PacedFallback,
}

/// Distinguishes native notifications from cancellation checkpoints and paced
/// fallback ticks. Only `PacedFallback` authorizes a state reread without an
/// event; native timeouts merely let callers check cancellation and deadlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationWake {
    Changed,
    NativeTimeout,
    Woken,
    PacedFallback,
    Cancelled,
}

/// One host event source for all local observation paths, with an explicit
/// configured-interval fallback when native registration is unavailable.
pub(crate) struct LocalObservation {
    watcher: Option<provenant_os::changes::watch::Watcher>,
    interval: Duration,
    fallback: Option<String>,
    has_local_paths: bool,
    next_paced: Option<provenant_core::MonoTime>,
}
impl LocalObservation {
    pub(crate) fn subscribe(paths: &[PathBuf], interval: Duration) -> Self {
        if paths.is_empty() {
            return Self {
                watcher: None,
                interval,
                fallback: Some("no local event targets".into()),
                has_local_paths: false,
                next_paced: None,
            };
        }
        match provenant_os::changes::watch::Watcher::subscribe(paths) {
            Ok(watcher) => Self {
                watcher: Some(watcher),
                interval,
                fallback: None,
                has_local_paths: true,
                next_paced: None,
            },
            Err(error) => Self {
                watcher: None,
                interval,
                fallback: Some(error.to_string()),
                has_local_paths: true,
                next_paced: None,
            },
        }
    }

    pub(crate) fn paced(interval: Duration, reason: impl Into<String>) -> Self {
        Self {
            watcher: None,
            interval,
            fallback: Some(reason.into()),
            has_local_paths: true,
            next_paced: None,
        }
    }

    pub(crate) fn interval(&self) -> Duration {
        self.interval
    }

    pub(crate) fn report(&self, remote_pending: bool) -> String {
        if !self.has_local_paths {
            return if remote_pending {
                format!(
                    "peer statuses reread every {} ms (domains.wait.local_interval_ms); no local event targets",
                    self.interval.as_millis()
                )
            } else {
                "no local event targets".into()
            };
        }
        observation_text_with_remote(
            self.watcher.as_ref(),
            self.interval,
            self.fallback.as_deref(),
            remote_pending,
        )
    }

    pub(crate) fn wait(
        &mut self,
        maximum: Duration,
        cancel: &CancelToken,
        clock: &(impl provenant_core::Clock + ?Sized),
    ) -> Result<ObservationWake, provenant_core::TimeError> {
        if cancel.is_cancelled() {
            return Ok(ObservationWake::Cancelled);
        }
        let slice = maximum.min(Duration::from_millis(250));
        let native_deadline = if self.watcher.is_some() {
            Some(
                clock
                    .mono_now()
                    .checked_add(provenant_core::DurationMicros::from_micros(
                        slice.as_micros() as u64,
                    ))?,
            )
        } else {
            None
        };
        if let Some(deadline) = native_deadline {
            let result = match self.watcher.as_mut() {
                Some(watcher) => watcher.wait(slice),
                None => return self.wait_paced(maximum, cancel, clock),
            };
            if cancel.is_cancelled() {
                return Ok(ObservationWake::Cancelled);
            }
            match result {
                Ok(provenant_os::changes::watch::WaitOutcome::Changed) => {
                    return Ok(ObservationWake::Changed);
                }
                Ok(provenant_os::changes::watch::WaitOutcome::TimedOut) => {
                    // Native waits use real kernel time. Advance injected clocks
                    // to the same pre-wait checkpoint without rereading state.
                    if !slice.is_zero() {
                        clock.wait_until(deadline)?;
                    }
                    return Ok(ObservationWake::NativeTimeout);
                }
                Ok(provenant_os::changes::watch::WaitOutcome::Woken) => {
                    return Ok(ObservationWake::Woken);
                }
                Err(error) => {
                    // A failed native source becomes an explicit, reported
                    // paced fallback. Do not reread until its configured tick.
                    self.watcher = None;
                    self.fallback = Some(error.to_string());
                }
            }
        }
        self.wait_paced(maximum, cancel, clock)
    }

    fn wait_paced(
        &mut self,
        maximum: Duration,
        cancel: &CancelToken,
        clock: &(impl provenant_core::Clock + ?Sized),
    ) -> Result<ObservationWake, provenant_core::TimeError> {
        let interval =
            provenant_core::DurationMicros::from_micros(self.interval.as_micros() as u64);
        let due = match self.next_paced {
            Some(due) => due,
            None => {
                let due = clock.mono_now().checked_add(interval)?;
                self.next_paced = Some(due);
                due
            }
        };
        let now = clock.mono_now();
        if now >= due {
            self.next_paced = Some(now.checked_add(interval)?);
            return Ok(ObservationWake::PacedFallback);
        }

        let requested =
            clock
                .mono_now()
                .checked_add(provenant_core::DurationMicros::from_micros(
                    maximum.as_micros().min(u64::MAX as u128) as u64,
                ))?;
        let wake_at = requested.min(due);
        while !cancel.is_cancelled() {
            let now = clock.mono_now();
            if now >= wake_at {
                break;
            }
            let remaining = wake_at.as_micros().saturating_sub(now.as_micros());
            let slice = Duration::from_micros(remaining).min(Duration::from_millis(250));
            let slice_deadline = now.checked_add(provenant_core::DurationMicros::from_micros(
                slice.as_micros() as u64,
            ))?;
            clock.wait_until(slice_deadline)?;
        }
        if cancel.is_cancelled() {
            return Ok(ObservationWake::Cancelled);
        }
        let now = clock.mono_now();
        if now >= due {
            // Start the next period at this observed tick, preserving a single
            // reread even if a remote wait shortened each preceding call.
            self.next_paced = Some(now.checked_add(interval)?);
            Ok(ObservationWake::PacedFallback)
        } else {
            Ok(ObservationWake::NativeTimeout)
        }
    }
}

fn graph_reads_for_wake(
    pending: &[usize],
    remote: &std::collections::BTreeSet<usize>,
    wake: GraphWake,
) -> Vec<usize> {
    match wake {
        GraphWake::Event => pending
            .iter()
            .copied()
            .filter(|index| !remote.contains(index))
            .collect(),
        GraphWake::NativeTimeout => Vec::new(),
        GraphWake::PacedFallback => pending.to_vec(),
    }
}

fn include_due_remote_reads(
    reads: &mut Vec<usize>,
    pending: &[usize],
    remote: &std::collections::BTreeSet<usize>,
    remote_poll_due: bool,
) {
    if remote_poll_due {
        let due = pending
            .iter()
            .copied()
            .filter(|index| remote.contains(index) && !reads.contains(index))
            .collect::<Vec<_>>();
        reads.extend(due);
    }
}

fn observation_text_with_remote(
    watcher: Option<&provenant_os::changes::watch::Watcher>,
    interval: Duration,
    fallback: Option<&str>,
    remote_pending: bool,
) -> String {
    let mut text = observation_text(watcher, interval, fallback);
    if remote_pending {
        text.push_str(&format!(
            "; peer/relay or unrepresented rows reread every {} ms (domains.wait.local_interval_ms) because the relay has no remote event subscription",
            interval.as_millis()
        ));
    }
    text
}

#[derive(Default)]
struct ObservationHistory {
    remote_source_seen: bool,
}
impl ObservationHistory {
    fn record(&mut self, reading: &Reading, host: Option<&provenant_core::HostId>) {
        self.remote_source_seen |= !reading_locally_represented(reading, host);
    }

    fn render(
        &self,
        watcher: Option<&provenant_os::changes::watch::Watcher>,
        interval: Duration,
        fallback: Option<&str>,
    ) -> String {
        observation_text_with_remote(watcher, interval, fallback, self.remote_source_seen)
    }
}

/// Only terminal node/attempt states produce settlement events. In particular
/// pending -> running is progress, not a reason for --any to return.
fn digest_settle(digest: &Value) -> Option<Settle> {
    if digest["settled"] == true {
        return Some(if digest["outcome"] == "succeeded" {
            Settle::Succeeded
        } else {
            Settle::Unsuccessful
        });
    }
    match digest["state"].as_str()? {
        "succeeded" => Some(Settle::Succeeded),
        "failed" | "blocked" | "skipped" | "unreadable" => Some(Settle::Unsuccessful),
        "terminal" => Some(if digest["outcome"] == "succeeded" {
            Settle::Succeeded
        } else {
            Settle::Unsuccessful
        }),
        _ => None,
    }
}

fn new_settlements<'a>(previous: Option<&Reading>, reading: &'a Reading) -> Vec<&'a Value> {
    reading.data["digests"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|digest| {
            digest_settle(digest).is_some()
                && !previous.is_some_and(|previous| {
                    previous.data["digests"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|old| old["key"] == digest["key"] && digest_settle(old).is_some())
                })
        })
        .collect()
}

fn settlement_verdict(digests: &[&Value]) -> Option<Settle> {
    if digests
        .iter()
        .any(|digest| digest_settle(digest) == Some(Settle::Unsuccessful))
    {
        Some(Settle::Unsuccessful)
    } else if !digests.is_empty() {
        Some(Settle::Succeeded)
    } else {
        None
    }
}

/// Fold source readings until the wait is done: under `any`, the first target
/// that settles or needs input; otherwise every target. Returns false when the
/// deadline passes first. `settled` records the order targets finished in, and
/// `changed` sees each distinct reading once.
fn collect(
    receiver: &mpsc::Receiver<Update>,
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    deadline: Instant,
    any: bool,
    changed: &mut impl FnMut(usize, &Reading),
) -> bool {
    loop {
        let finished = |index: &usize| latest[*index].as_ref().is_some_and(Reading::done);
        let done = if any {
            !settled.is_empty()
        } else {
            (0..latest.len()).all(|index| finished(&index))
        };
        if done {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(mut update) = receiver.recv_timeout(left) else {
            for reading in latest.iter_mut().flatten() {
                if !reading.done() && reading.data.is_object() {
                    reading.data["stale"] = json!(true);
                }
            }
            return false;
        };
        let index = update.target;
        if update.reading.data.is_null()
            && let Some(previous) = latest[index]
                .as_ref()
                .filter(|reading| reading.data.is_object())
        {
            update.reading.data = previous.data.clone();
            update.reading.data["stale"] = json!(true);
        }
        if latest[index].as_ref() == Some(&update.reading) {
            continue;
        }
        let node_verdict = latest[index].as_ref().and_then(|previous| {
            settlement_verdict(&new_settlements(Some(previous), &update.reading))
        });
        changed(index, &update.reading);
        if any
            && !update.reading.done()
            && let Some(verdict) = node_verdict
        {
            update.reading.settle = verdict;
        }
        if update.reading.done() && !settled.contains(&index) {
            settled.push(index);
        }
        latest[index] = Some(update.reading);
    }
}

/// How the wait ended: a timeout, else the worst of the targets that decided
/// it. Under `any` only the first target to finish decides.
fn outcome(latest: &[Option<Reading>], settled: &[usize], done: bool, any: bool) -> Outcome {
    if !done {
        return Outcome::Timeout;
    }
    let deciding = if any {
        &settled[..settled.len().min(1)]
    } else {
        settled
    };
    let settles: Vec<_> = deciding
        .iter()
        .filter_map(|index| latest[*index].as_ref().map(|reading| reading.settle))
        .collect();
    if settles.contains(&Settle::Unsuccessful) {
        Outcome::Failed
    } else if settles.contains(&Settle::NeedsInput) {
        Outcome::Pending
    } else {
        Outcome::Ok
    }
}

fn preflight_error(
    error: &(dyn std::error::Error + 'static),
    deadline: Instant,
    cancel: &CancelToken,
    globals: &Globals,
) -> u8 {
    if cancel.is_cancelled() {
        crate::fail(
            "wait",
            Outcome::Cancelled,
            "CANCELLED",
            "wait cancelled; the work continues",
            globals,
        )
    } else if Instant::now() >= deadline {
        crate::fail(
            "wait",
            Outcome::Timeout,
            "TIMEOUT",
            "wait timed out; the work continues",
            globals,
        )
    } else {
        crate::fabric::command_fail("wait", error, globals)
    }
}

pub(crate) fn run(command: provenant_cli::Wait, globals: &Globals) -> u8 {
    use crate::roots::{Product, Workspace};
    let resolved = match crate::roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail("wait", Outcome::Error, "ROOT", &error.to_string(), globals);
        }
    };
    let settings = match provenant_cli::config_reads::read_for(&resolved.roots, "wait") {
        Ok(config) => config.domains.wait,
        Err(error) => {
            return crate::fail(
                "wait",
                Outcome::Error,
                "CONFIG",
                &error.to_string(),
                globals,
            );
        }
    };
    let edge = provenant_cli::run::Edge {
        roots: &resolved.roots,
        home: &resolved.home,
        assets: &resolved.assets,
        cwd: &resolved.project,
    };
    let deadline = Instant::now() + Duration::from_secs(settings.timeout(command.timeout));
    let stop = Stop::default();
    let interval = Duration::from_millis(settings.local_interval_ms);
    let mut sources: Vec<Source> = command.targets.iter().cloned().map(Source::Graph).collect();

    // Subscribe before the first status read so settlement during preflight
    // remains queued for the single local graph worker.
    let project = if provenant_cli::fabric::in_lane() {
        Err(
            "lane targets are read through their owner; local event files are unavailable"
                .to_owned(),
        )
    } else {
        provenant_cli::fabric::Context::open_read_only(
            resolved.roots.clone(),
            &resolved.home,
            &resolved.project,
        )
        .map(|context| (context.identity.project, context.identity.host))
        .map_err(|error| format!("cannot resolve project event logs: {error}"))
    };
    let local_host = project.as_ref().ok().map(|(_, host)| host.clone());
    let project_error = project.as_ref().err().cloned();
    let mut pending_index_error = None;
    let event_paths = match project {
        Ok((project, _)) if !command.targets.is_empty() => command
            .targets
            .iter()
            .map(|target| graph_event_path(resolved.roots.state.as_path(), project, &target.name))
            .collect::<Vec<_>>(),
        Ok((project, _)) if command.prs.is_empty() => {
            let names = provenant_fabric_runtime::graph::FileJournal::pending_graph_ids(
                &resolved.roots.state,
                project,
            )
            .map_err(|error| format!("cannot resolve pending graph events: {error}"));
            match names {
                Ok(names) => {
                    let names = names
                        .into_iter()
                        .take(settings.max_targets as usize)
                        .collect::<Vec<_>>();
                    if names.is_empty() {
                        vec![graph_event_path(
                            resolved.roots.state.as_path(),
                            project,
                            ".wait",
                        )]
                    } else {
                        names
                            .iter()
                            .map(|name| {
                                graph_event_path(resolved.roots.state.as_path(), project, name)
                            })
                            .collect()
                    }
                }
                Err(error) => {
                    pending_index_error = Some(error);
                    Vec::new()
                }
            }
        }
        _ => Vec::new(),
    };
    let setup_fallback = project_error.or(pending_index_error);
    let (mut event_watcher, initial_fallback) = if event_paths.is_empty() {
        (
            None,
            setup_fallback.unwrap_or_else(|| "no local graph targets".to_owned()),
        )
    } else {
        match provenant_os::changes::watch::Watcher::subscribe(&event_paths) {
            Ok(watcher) => (Some(watcher), String::new()),
            Err(error) => (None, setup_fallback.unwrap_or_else(|| error.to_string())),
        }
    };
    let fallback_reason = event_watcher.is_none().then(|| initial_fallback.clone());
    if let Some(watcher) = &event_watcher {
        stop.install_event_wake(watcher.wake_handle());
    }
    let observation = Arc::new(Mutex::new(observation_text_with_remote(
        event_watcher.as_ref(),
        interval,
        fallback_reason.as_deref(),
        false,
    )));
    let mut validation = std::collections::BTreeMap::new();
    if let Err(error) = provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
        crate::fabric::check_targets(
            &command.targets,
            &edge,
            deadline,
            &stop.cancel,
            |target, rows| {
                validation.insert(
                    target.to_string(),
                    classify_rows(&target.to_string(), &rows),
                );
            },
        )
    }) {
        return preflight_error(error.as_ref(), deadline, &stop.cancel, globals);
    }
    if command.targets.is_empty() && command.prs.is_empty() {
        if provenant_cli::fabric::in_lane() {
            return crate::fail(
                "wait",
                Outcome::Invalid,
                "TARGET_REQUIRED",
                "inside a lane, wait names its targets: NAME[/NODE[#N]] or --pr NUMBER",
                globals,
            );
        }
        match provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
            crate::fabric::unsettled_graphs(&edge, deadline, &stop.cancel)
        }) {
            Ok(graphs) => {
                for graph in graphs {
                    if graph.error.is_none() {
                        validation.insert(
                            graph.target.to_string(),
                            classify_rows(&graph.target.to_string(), &graph.rows),
                        );
                    }
                    sources.push(match graph.error {
                        Some(error) => Source::UnreadableGraph(graph.target, error),
                        None => Source::Graph(graph.target),
                    });
                }
            }
            Err(error) => return preflight_error(error.as_ref(), deadline, &stop.cancel, globals),
        }
    }
    let mut prs = command.prs.clone();
    prs.sort_unstable();
    prs.dedup();
    let pr_read_timeout = Duration::from_secs(settings.pr_read_timeout_seconds);
    if sources.len() + prs.len() > settings.max_targets as usize {
        return crate::fail(
            "wait",
            Outcome::Invalid,
            "TOO_MANY_TARGETS",
            &format!("wait takes at most {} targets", settings.max_targets),
            globals,
        );
    }
    if !prs.is_empty() {
        let repository = crate::pr::status::wait_repository(
            edge.roots.state.as_path(),
            &edge.cwd,
            pr_read_timeout,
        )
        .map_err(|error| error.to_string());
        sources.extend(prs.into_iter().map(|number| Source::Pr {
            number,
            repository: repository.clone(),
        }));
    }
    let mut latest: Vec<Option<Reading>> = sources
        .iter()
        .map(|source| validation.get(&source.label()).cloned())
        .collect();
    let mut settled: Vec<usize> = latest
        .iter()
        .enumerate()
        .filter_map(|(index, reading)| reading.as_ref().is_some_and(Reading::done).then_some(index))
        .collect();
    let mut write_error = None;
    let mut announced_targets = std::collections::BTreeSet::new();
    let mut announced_nodes = std::collections::BTreeSet::new();
    let mut observed_targets = std::collections::BTreeSet::new();
    let mut transition = |index: usize, reading: &Reading| {
        let target_settled = reading.done() && announced_targets.insert(index);
        let first_sample = observed_targets.insert(index);
        let node_settled: Vec<_> = new_settlements(None, reading)
            .into_iter()
            .filter(|digest| {
                let newly_observed = announced_nodes.insert((index, digest["key"].to_string()));
                newly_observed && (!first_sample || target_settled)
            })
            .collect();
        let event = if target_settled {
            "settled"
        } else if !node_settled.is_empty() {
            "node_settled"
        } else {
            "transition"
        };
        if (target_settled || !node_settled.is_empty() || command.follow) && write_error.is_none() {
            let line = if globals.json {
                json!({"event":event,"target":sources[index].label(),
                    "settle":settlement_verdict(&node_settled).unwrap_or(reading.settle),"line":reading.line,"digests":reading.data["digests"]})
                .to_string()
            } else {
                let mut line = format!("{event} {}", reading.line);
                for digest in reading.data["digests"].as_array().into_iter().flatten() {
                    if let Some(digest_line) = digest["line"].as_str() {
                        line.push('\n');
                        line.push_str(digest_line);
                    }
                }
                line
            };
            write_error = write_line(&line).err();
        }
    };
    for (index, reading) in latest.iter().enumerate() {
        if let Some(reading) = reading {
            transition(index, reading);
        }
    }
    let (sender, receiver) = mpsc::sync_channel(128);
    let graph_indices = sources
        .iter()
        .enumerate()
        .filter_map(|(index, source)| match source {
            Source::Graph(_) | Source::UnreadableGraph(..) => Some(index),
            Source::Pr { .. } => None,
        })
        .collect::<Vec<_>>();
    let done = std::thread::scope(|scope| {
        if !graph_indices.is_empty() {
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let observation = Arc::clone(&observation);
            let pending = graph_indices
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index]
                        .as_ref()
                        .is_none_or(|reading| !reading.done())
                })
                .collect::<Vec<_>>();
            let initial_reads = pending
                .iter()
                .copied()
                .filter(|index| latest[*index].is_none())
                .collect::<Vec<_>>();
            let graph_sources = graph_indices
                .iter()
                .map(|index| (*index, sources[*index].clone()))
                .collect::<Vec<_>>();
            let local_host = local_host.clone();
            let mut watcher = event_watcher.take();
            let mut fallback_reason = fallback_reason.clone();
            let mut remote_indices = pending
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index].as_ref().is_none_or(|reading| {
                        !reading_locally_represented(reading, local_host.as_ref())
                    })
                })
                .collect::<std::collections::BTreeSet<_>>();
            let mut observation_history = ObservationHistory::default();
            for index in &pending {
                if let Some(reading) = latest[*index].as_ref() {
                    observation_history.record(reading, local_host.as_ref());
                }
            }
            if let Ok(mut current) = observation.lock() {
                *current = observation_history.render(
                    watcher.as_ref(),
                    interval,
                    fallback_reason.as_deref(),
                );
            }
            scope.spawn(move || {
                let mut session = None;
                let mut observation_history = observation_history;
                let mut pending = pending;
                // Read once for rows that arrived after preflight or were not
                // represented in its sample before waiting for an event.
                for index in initial_reads {
                    let Some((_, source)) = graph_sources
                        .iter()
                        .find(|(candidate, _)| *candidate == index)
                    else {
                        continue;
                    };
                    let reading = read(
                        source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                    );
                    observation_history.record(&reading, local_host.as_ref());
                    let locally_represented =
                        reading_locally_represented(&reading, local_host.as_ref());
                    if locally_represented {
                        remote_indices.remove(&index);
                    } else {
                        remote_indices.insert(index);
                    }
                    let finished = reading.done();
                    if finished {
                        remote_indices.remove(&index);
                        pending.retain(|candidate| *candidate != index);
                    }
                    if let Ok(mut current) = observation.lock() {
                        *current = observation_history.render(
                            watcher.as_ref(),
                            interval,
                            fallback_reason.as_deref(),
                        );
                    }
                    if sender
                        .send(Update {
                            target: index,
                            reading,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                let mut next_remote_poll = Instant::now() + interval;
                while !pending.is_empty() && !stop.cancel.is_cancelled() {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    let wake = if let Some(source) = watcher.as_mut() {
                        let until_remote = if remote_indices.is_empty() {
                            left
                        } else {
                            next_remote_poll.saturating_duration_since(Instant::now())
                        };
                        let wait_for = left.min(until_remote);
                        match source.wait(wait_for) {
                            Ok(provenant_os::changes::watch::WaitOutcome::Changed) => {
                                Some(GraphWake::Event)
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::TimedOut) => {
                                if deadline.saturating_duration_since(Instant::now()).is_zero() {
                                    break;
                                }
                                Some(GraphWake::NativeTimeout)
                            }
                            Ok(provenant_os::changes::watch::WaitOutcome::Woken) => break,
                            Err(error) => {
                                watcher = None;
                                fallback_reason = Some(format!("event source failed: {error}"));
                                if let Ok(mut current) = observation.lock() {
                                    *current = observation_history.render(
                                        None,
                                        interval,
                                        fallback_reason.as_deref(),
                                    );
                                }
                                let left = deadline.saturating_duration_since(Instant::now());
                                if !stop.pause(interval.min(left)) {
                                    break;
                                }
                                Some(GraphWake::PacedFallback)
                            }
                        }
                    } else {
                        if !stop.pause(interval.min(left)) {
                            break;
                        }
                        Some(GraphWake::PacedFallback)
                    };
                    let Some(wake) = wake else {
                        continue;
                    };
                    // A local event may race with the remote cadence boundary.
                    // Include remote targets whenever that absolute deadline has
                    // elapsed, regardless of how often local events arrive.
                    let remote_poll_due =
                        !remote_indices.is_empty() && Instant::now() >= next_remote_poll;
                    let mut reads = graph_reads_for_wake(&pending, &remote_indices, wake);
                    include_due_remote_reads(
                        &mut reads,
                        &pending,
                        &remote_indices,
                        remote_poll_due,
                    );
                    let polled_remote = reads.iter().any(|index| remote_indices.contains(index));
                    for index in reads {
                        if stop.cancel.is_cancelled() {
                            break;
                        }
                        let Some((_, source)) = graph_sources
                            .iter()
                            .find(|(candidate, _)| *candidate == index)
                        else {
                            continue;
                        };
                        let reading = read(
                            source,
                            edge,
                            &mut session,
                            deadline,
                            pr_read_timeout,
                            &stop.cancel,
                        );
                        observation_history.record(&reading, local_host.as_ref());
                        let locally_represented =
                            reading_locally_represented(&reading, local_host.as_ref());
                        if locally_represented {
                            remote_indices.remove(&index);
                        } else {
                            remote_indices.insert(index);
                        }
                        let finished = reading.done();
                        if finished {
                            remote_indices.remove(&index);
                            pending.retain(|candidate| *candidate != index);
                        }
                        if let Ok(mut current) = observation.lock() {
                            *current = observation_history.render(
                                watcher.as_ref(),
                                interval,
                                fallback_reason.as_deref(),
                            );
                        }
                        if sender
                            .send(Update {
                                target: index,
                                reading,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    if polled_remote {
                        next_remote_poll = Instant::now() + interval;
                    }
                }
            });
        }
        for (index, source) in sources.iter().enumerate() {
            if !matches!(source, Source::Pr { .. })
                || latest[index].as_ref().is_some_and(Reading::done)
            {
                continue;
            }
            let sampled = latest[index].is_some();
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let interval = Duration::from_millis(settings.pr_interval_ms);
            scope.spawn(move || {
                if sampled
                    && !stop.pause(interval.min(deadline.saturating_duration_since(Instant::now())))
                {
                    return;
                }
                let mut session = None;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if stop.cancel.is_cancelled() {
                        break;
                    }
                    let reading = read(
                        source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                    );
                    let finished = reading.done();
                    if sender
                        .send(Update {
                            target: index,
                            reading,
                        })
                        .is_err()
                        || finished
                        || left.is_zero()
                        || !stop
                            .pause(interval.min(deadline.saturating_duration_since(Instant::now())))
                    {
                        break;
                    }
                }
            });
        }
        drop(sender);
        let done = collect(
            &receiver,
            &mut latest,
            &mut settled,
            deadline,
            command.any,
            &mut transition,
        );
        stop.stop();
        drop(receiver);
        done
    });
    if let Some(error) = write_error {
        return crate::fail("wait", Outcome::Error, "IO", &error.to_string(), globals);
    }
    let outcome = outcome(&latest, &settled, done, command.any);
    let mut human = String::new();
    let rows: Vec<_> = sources
        .iter()
        .zip(&latest)
        .map(|(source, reading)| {
            let label = source.label();
            let line = reading.as_ref().map_or_else(
                || format!("{label} pending"),
                |reading| reading.line.clone(),
            );
            human.push_str(&line);
            human.push('\n');
            if let Some(reading) = reading {
                for digest in reading.data["digests"].as_array().into_iter().flatten() {
                    if let Some(line) = digest["line"].as_str() {
                        human.push_str(line);
                        human.push('\n');
                    }
                }
            }
            json!({
                "target":label,
                "settle":reading.as_ref().map_or(Settle::Pending, |reading| reading.settle),
                "line":line,
                "data":reading.as_ref().map(|reading| &reading.data),
            })
        })
        .collect();
    if sources.is_empty() {
        human.push_str("nothing to wait for: no unsettled graphs\n");
    }
    if !done {
        human.push_str("wait timed out; the work continues\n");
    }
    let observation = observation
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "event source status unavailable".into());
    if !graph_indices.is_empty() {
        human.push_str(&format!("local observation: {observation}\n"));
    }
    crate::emit(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            json!({"targets":rows,"timed_out":!done,"local_observation":if graph_indices.is_empty(){Value::Null}else{json!(observation)}}),
        ),
        globals,
        &human,
        true,
    )
}

fn write_line(line: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")?;
    stdout.flush()
}

#[cfg(test)]
#[allow(dead_code)] // Also included by the worktree driver without libtest.
mod tests {
    use super::*;

    #[test]
    fn a_selection_with_peer_or_relayed_attempts_is_not_locally_represented() {
        let row = |id: &str| json!({"kind": "attempt", "attempt_id": id});
        let here = "aa".to_owned();
        assert!(rows_local(
            &[row("aa/1"), json!({"kind": "graph"})],
            Some(&here)
        ));
        assert!(!rows_local(&[row("aa/1"), row("bb/2")], Some(&here)));
        assert!(!rows_local::<String>(&[row("aa/1")], None));
    }

    #[test]
    fn completed_peer_source_remains_in_final_observation() {
        let host = "aa".to_owned();
        let completed_peer = Reading {
            settle: Settle::Succeeded,
            line: "peer settled".into(),
            data: json!({"rows":[{"kind":"attempt","attempt_id":"bb/2"}]}),
        };
        let mut history = ObservationHistory::default();
        history.record(&completed_peer, Some(&host));

        let final_observation = history.render(
            None,
            Duration::from_millis(500),
            Some("event source unavailable: fixture parent missing"),
        );
        assert!(completed_peer.done());
        assert!(final_observation.contains("peer/relay or unrepresented rows reread every 500 ms"));
    }

    #[test]
    fn local_observation_reports_the_fallback_source_and_reason() {
        let observation = LocalObservation::paced(
            Duration::from_millis(40),
            "fixture has no native event backend",
        );
        let report = observation.report(false);
        assert!(report.contains("domains.wait.local_interval_ms"));
        assert!(report.contains("fixture has no native event backend"));
    }

    #[test]
    fn repeated_short_waits_eventually_reach_one_paced_fallback_tick() {
        let interval = Duration::from_millis(40);
        let mut observation = LocalObservation::paced(interval, "fixture event source unavailable");
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
            provenant_core::MonoTime::from_micros(0),
        );
        for _ in 0..3 {
            assert_eq!(
                observation
                    .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
                    .expect("fixture clock can advance"),
                ObservationWake::NativeTimeout
            );
        }
        assert_eq!(
            observation
                .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
                .expect("fixture clock can advance"),
            ObservationWake::PacedFallback
        );
        assert_eq!(clock.mono_now().as_micros(), 40_000);
        assert_eq!(
            observation
                .wait(Duration::from_millis(10), &CancelToken::new(), &clock)
                .expect("next cadence remains paced"),
            ObservationWake::NativeTimeout
        );
        assert_eq!(clock.mono_now().as_micros(), 50_000);
    }

    #[test]
    fn registration_does_not_create_an_event_log_and_empty_timeout_is_not_a_read() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let log = directory.path().join("graph.events.jsonl");
        let mut observation = LocalObservation::subscribe(&[log.clone()], Duration::from_secs(1));
        assert!(!log.exists());
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
            provenant_core::MonoTime::from_micros(0),
        );
        let wake = observation
            .wait(Duration::ZERO, &CancelToken::new(), &clock)
            .expect("zero deadline does not touch the clock");
        assert_eq!(wake, ObservationWake::NativeTimeout);
    }

    #[test]
    fn a_short_fallback_deadline_is_not_mistaken_for_a_paced_tick() {
        let mut observation = LocalObservation::paced(
            Duration::from_secs(1),
            "fixture has no native event backend",
        );
        let clock = provenant_core::TestClock::new(
            provenant_core::WallTime::from_micros(0).expect("valid fixture time"),
            provenant_core::MonoTime::from_micros(0),
        );
        let wake = observation
            .wait(Duration::ZERO, &CancelToken::new(), &clock)
            .expect("zero deadline does not touch the clock");
        assert_eq!(wake, ObservationWake::NativeTimeout);
    }

    #[test]
    fn graph_events_read_only_local_sources_and_remote_deadlines_add_remote_sources() {
        let pending = [0, 1, 2];
        let remote = [1, 2].into_iter().collect();
        let as_set = |indices: Vec<usize>| {
            indices
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
        };
        let event_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
        assert_eq!(as_set(event_reads.clone()), [0].into_iter().collect());
        let native_timeout = graph_reads_for_wake(&pending, &remote, GraphWake::NativeTimeout);
        assert!(native_timeout.is_empty());
        let mut due_event_reads = event_reads;
        include_due_remote_reads(&mut due_event_reads, &pending, &remote, true);
        assert_eq!(as_set(due_event_reads), pending.into_iter().collect());
        let mut due_timeout_reads = native_timeout;
        include_due_remote_reads(&mut due_timeout_reads, &pending, &remote, true);
        assert_eq!(as_set(due_timeout_reads), [1, 2].into_iter().collect());
        assert_eq!(
            as_set(graph_reads_for_wake(
                &pending,
                &remote,
                GraphWake::PacedFallback
            )),
            pending.into_iter().collect()
        );
    }

    #[test]
    fn a_flood_of_local_events_cannot_starve_a_due_remote_poll() {
        let pending = [0, 1];
        let remote = [1].into_iter().collect();
        let origin = Instant::now();
        let interval = Duration::from_millis(100);
        let next_remote_poll = origin + interval;

        for elapsed in (0..100).step_by(10) {
            let now = origin + Duration::from_millis(elapsed);
            let mut reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
            include_due_remote_reads(&mut reads, &pending, &remote, now >= next_remote_poll);
            assert_eq!(reads, [0]);
        }
        let due = origin + interval;
        let mut due_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
        include_due_remote_reads(&mut due_reads, &pending, &remote, due >= next_remote_poll);
        assert_eq!(due_reads, pending);
        let next_remote_poll = due + interval;
        let mut next_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
        include_due_remote_reads(
            &mut next_reads,
            &pending,
            &remote,
            due + Duration::from_millis(99) >= next_remote_poll,
        );
        assert_eq!(next_reads, [0]);
        let mut second_due_reads = graph_reads_for_wake(&pending, &remote, GraphWake::Event);
        include_due_remote_reads(
            &mut second_due_reads,
            &pending,
            &remote,
            due + interval >= next_remote_poll,
        );
        assert_eq!(second_due_reads, pending);
    }

    #[test]
    fn failed_reread_retains_the_last_snapshot_as_stale() {
        for failure_delivered in [false, true] {
            let rows = json!({"rows":[{"kind":"graph","state":"pending"}]});
            let mut latest = vec![Some(Reading {
                settle: Settle::Pending,
                line: "g pending".into(),
                data: rows.clone(),
            })];
            let (sender, receiver) = mpsc::channel();
            if failure_delivered {
                sender
                    .send(Update {
                        target: 0,
                        reading: Reading::unavailable("g", "relay timed out"),
                    })
                    .unwrap();
            }
            drop(sender);
            let mut settled = Vec::new();
            assert!(!collect(
                &receiver,
                &mut latest,
                &mut settled,
                Instant::now(),
                false,
                &mut |_, _| {}
            ));
            let last = latest[0].as_ref().unwrap();
            assert_eq!(last.data["rows"], rows["rows"]);
            assert_eq!(last.data["stale"], true);
            assert_eq!(last.settle, Settle::Pending);
            assert!(settled.is_empty());
        }
    }

    #[test]
    fn bare_wait_reports_an_unreadable_graph_as_unsuccessful()
    -> Result<(), Box<dyn std::error::Error>> {
        if crate::fabric::peer_tests::host_test(
            "status_wait::tests::bare_wait_reports_an_unreadable_graph_as_unsuccessful",
        )? {
            return Ok(());
        }

        use provenant_core::Clock;
        use provenant_fabric_runtime::graph::FileJournal;
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let context = provenant_cli::fabric::Context::open(roots.clone(), &rig.home, &rig.project)?;
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = CancelToken::new();
        drop(FileJournal::open(
            &roots.state,
            context.identity.project,
            "broken",
            provenant_core::WaitBudget::cli(clock.mono_now(), 30)?,
            &cancel,
            &clock,
        )?);
        std::fs::write(
            roots
                .state
                .as_path()
                .join("fabric/graphs")
                .join(context.identity.project.to_string())
                .join("graph-broken.json"),
            b"invalid checkpoint",
        )?;
        let edge = provenant_cli::run::Edge {
            roots: &roots,
            home: &rig.home,
            assets: &rig.project,
            cwd: &rig.project,
        };
        let deadline = Instant::now() + Duration::from_secs(30);
        let discovered = crate::fabric::unsettled_graphs(&edge, deadline, &cancel)?;
        let graph = discovered
            .into_iter()
            .find(|graph| graph.target.name == "broken")
            .ok_or("broken graph was hidden")?;
        let reading = read(
            &Source::UnreadableGraph(graph.target, graph.error.ok_or("decode failure")?),
            &edge,
            &mut None,
            deadline,
            Duration::from_secs(1),
            &cancel,
        );
        assert_eq!(reading.settle, Settle::Unsuccessful);
        assert!(reading.done());
        Ok(())
    }

    // File-stamp gates were removed with local polling; native event wake
    // coverage lives beside the OS port, with paced fallback exercised by reads.

    // Local-stamp ownership checks were removed with file polling. Native graph
    // event subscription is covered by the OS port's multi-log invariant.

    #[test]
    fn preflight_expiry_and_cancellation_use_wait_exit_codes() {
        let cancel = CancelToken::new();
        let globals = Globals::default();
        let error = provenant_fabric_runtime::coordination::facade::FacadeError::Timeout;
        assert_eq!(
            preflight_error(&error, Instant::now(), &cancel, &globals),
            124
        );
        cancel.cancel();
        assert_eq!(
            preflight_error(
                &error,
                Instant::now() + Duration::from_secs(30),
                &cancel,
                &globals
            ),
            130
        );
    }

    #[test]
    fn graph_wait_readers_share_the_callers_deadline_and_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        let rig = crate::worktree_tests::repository()?;
        let roots = crate::roots::child(&rig.home, &rig.project, rig.state.clone())?;
        let edge = provenant_cli::run::Edge {
            roots: &roots,
            home: &rig.home,
            assets: &rig.project,
            cwd: &rig.project,
        };
        let target = "missing".parse()?;
        for cancelled in [false, true] {
            let cancel = CancelToken::new();
            let deadline = if cancelled {
                cancel.cancel();
                Instant::now() + Duration::from_secs(30)
            } else {
                Instant::now()
            };
            for error in [
                crate::fabric::check_targets(
                    std::slice::from_ref(&target),
                    &edge,
                    deadline,
                    &cancel,
                    |_, _| {},
                )
                .unwrap_err(),
                crate::fabric::unsettled_graphs(&edge, deadline, &cancel).unwrap_err(),
                crate::fabric::read_status_rows(&target, &edge, &mut None, deadline, &cancel)
                    .unwrap_err(),
            ] {
                assert!(matches!(
                    (cancelled, error.downcast_ref::<FacadeError>()),
                    (true, Some(FacadeError::Cancelled)) | (false, Some(FacadeError::Timeout))
                ));
            }
        }
        Ok(())
    }

    fn reading(settle: Settle) -> Reading {
        Reading {
            settle,
            line: format!("{settle:?}"),
            data: Value::Null,
        }
    }

    fn wait_on(updates: &[(usize, Settle)], count: usize, any: bool) -> (Outcome, Vec<usize>) {
        let (sender, receiver) = mpsc::channel();
        for (target, settle) in updates {
            assert!(
                sender
                    .send(Update {
                        target: *target,
                        reading: reading(*settle),
                    })
                    .is_ok()
            );
        }
        let mut latest = vec![None; count];
        let mut settled = Vec::new();
        let done = collect(
            &receiver,
            &mut latest,
            &mut settled,
            Instant::now() + Duration::from_millis(20),
            any,
            &mut |_, _| {},
        );
        (outcome(&latest, &settled, done, any), settled)
    }

    #[test]
    fn any_returns_at_the_first_settlement_and_takes_its_verdict() {
        use Settle::*;
        let (outcome, settled) = wait_on(
            &[
                (0, Pending),
                (1, Pending),
                (1, Unsuccessful),
                (0, Succeeded),
            ],
            2,
            true,
        );
        assert_eq!(settled, [1], "the later settlement was never read");
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(
            wait_on(&[(0, Pending), (1, NeedsInput)], 2, true).0,
            Outcome::Pending
        );
    }

    #[test]
    fn all_waits_for_every_target_and_a_timeout_is_124() {
        use Settle::*;
        let (outcome, _) = wait_on(&[(0, Succeeded), (1, Pending)], 2, false);
        assert_eq!(provenant_cli::exit_status(&outcome), 124);
        let (outcome, _) = wait_on(&[(0, Succeeded), (1, Pending), (1, Succeeded)], 2, false);
        assert_eq!(provenant_cli::exit_status(&outcome), 0);
        let (outcome, _) = wait_on(&[(0, NeedsInput), (1, Unsuccessful)], 2, false);
        assert_eq!(provenant_cli::exit_status(&outcome), 1);
        assert_eq!(provenant_cli::exit_status(&wait_on(&[], 1, true).0), 124);
    }

    #[test]
    fn a_target_settles_by_its_own_row() {
        let graph =
            |state: &str, outcome: &str| json!({"kind":"graph","state":state,"outcome":outcome});
        let node = |state: &str| json!({"kind":"node","state":state});
        let attempt =
            |state: &str, outcome: Value| json!({"kind":"attempt","state":state,"outcome":outcome});
        let settle = |rows: &[Value]| classify_rows("fix", rows).settle;
        assert_eq!(
            settle(&[graph("complete", "succeeded"), node("succeeded")]),
            Settle::Succeeded
        );
        assert_eq!(
            settle(&[graph("complete", "partial")]),
            Settle::Unsuccessful
        );
        assert_eq!(
            settle(&[graph("paused", ""), node("pending")]),
            Settle::NeedsInput
        );
        // A settled older attempt never decides a node that is running again.
        assert_eq!(
            settle(&[node("running"), attempt("terminal", json!("failed"))]),
            Settle::Pending
        );
        assert_eq!(
            settle(&[attempt("terminal", json!("succeeded"))]),
            Settle::Succeeded
        );
        assert_eq!(settle(&[attempt("running", Value::Null)]), Settle::Pending);
    }
}
