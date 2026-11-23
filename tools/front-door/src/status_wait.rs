//! `provenant wait`: block until graph targets or pull requests settle. A wait
//! is a reader: neither expiry nor cancellation stops the work. Local graphs
//! share one event source; pull requests remain on forge observation.
// This edge owns the real deadline for channel receives and source I/O.
#![allow(clippy::disallowed_methods)]
use provenant_cli::{Globals, wait::LocalCadence};
use provenant_core::CancelToken;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_records::Outcome;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
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
        /// The head this wait is pinned to. Until GitHub reports it, no
        /// verdict is taken from the pull request's current head.
        watch_head: Option<String>,
        complete: bool,
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
pub(crate) enum Settle {
    Pending,
    NeedsInput,
    Succeeded,
    Unsuccessful,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitMode {
    All,
    Any,
    FailFast,
}

impl WaitMode {
    fn from_command(command: &provenant_cli::Wait) -> Self {
        if command.any {
            Self::Any
        } else if command.fail_fast {
            Self::FailFast
        } else {
            Self::All
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
            Self::FailFast => "fail-fast",
        }
    }
}

fn mode_heading(mode: WaitMode) -> String {
    format!("wait mode: {}\n", mode.as_str())
}

fn terminal_data(
    mode: WaitMode,
    targets: Vec<Value>,
    timed_out: bool,
    local_observation: Option<&str>,
) -> Value {
    json!({
        "mode": mode.as_str(),
        "targets": targets,
        "timed_out": timed_out,
        "local_observation": local_observation,
    })
}

/// The settle phrase a chair reads in the final line. An unfinished target at
/// expiry is reported as still running at timeout, never as settled.
fn settle_phrase(settle: Settle, done: bool) -> &'static str {
    match settle {
        Settle::Succeeded => "settled ok",
        Settle::Unsuccessful => "failed",
        Settle::NeedsInput => "needs input",
        Settle::Pending if done => "still running",
        Settle::Pending => "still running at timeout",
    }
}

/// The one next command after a wait. Targets still running at timeout are the
/// ones a chair resumes; once every target has settled, a status re-read is the
/// neutral next step. The command is derived only from states just observed.
fn next_command(sources: &[Source], latest: &[Option<Reading>], superseding: &[String]) -> String {
    // A superseded name is still read as asked; the next command points at the
    // newer handle instead of re-reading the old one.
    if !superseding.is_empty() {
        return format!("provenant wait {}", superseding.join(" "));
    }
    let pending: Vec<_> = sources
        .iter()
        .zip(latest)
        .filter(|(_, reading)| {
            reading
                .as_ref()
                .is_some_and(|reading| reading.settle == Settle::Pending)
        })
        .map(|(source, _)| source.label())
        .collect();
    if !pending.is_empty() {
        return format!("provenant wait {}", pending.join(" "));
    }
    let targets: Vec<_> = sources.iter().map(Source::label).collect();
    if targets.is_empty() {
        "provenant status".to_owned()
    } else {
        format!("provenant status {}", targets.join(" "))
    }
}

/// Every target's settle state, in target order. Shared by the full human
/// line and the compact summary's `next` so both last lines agree.
fn settle_states(sources: &[Source], latest: &[Option<Reading>], done: bool) -> String {
    let states: Vec<_> = sources
        .iter()
        .zip(latest)
        .map(|(source, reading)| {
            let settle = reading
                .as_ref()
                .map_or(Settle::Pending, |reading| reading.settle);
            format!("{} {}", source.label(), settle_phrase(settle, done))
        })
        .collect();
    if states.is_empty() {
        "nothing to wait for".to_owned()
    } else {
        states.join(", ")
    }
}

/// The one next command, paired with the states in the summary's `next` line.
fn final_line(
    sources: &[Source],
    latest: &[Option<Reading>],
    done: bool,
    superseding: &[String],
) -> String {
    format!(
        "next: {}; {}\n",
        settle_states(sources, latest, done),
        next_command(sources, latest, superseding)
    )
}

/// The last known check summary and next step that `pr watch` used to print at
/// a timeout. `wait --pr` keeps them as an extra diagnostic line so the final
/// line stays owned by the wait final-line format.
fn pr_timeout_details(sources: &[Source], latest: &[Option<Reading>]) -> Vec<String> {
    sources
        .iter()
        .zip(latest)
        .filter_map(|(source, reading)| match (source, reading) {
            (Source::Pr { complete, .. }, Some(reading)) => {
                serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
                    .ok()
                    .filter(|line| line.wait_verdict(*complete).is_none())
                    .map(|line| {
                        let next = if line.checks_unreported() {
                            format!(
                                "; cause=checks_not_scheduled; next: {}",
                                crate::pr::status::NO_CHECKS_NEXT
                            )
                        } else {
                            String::new()
                        };
                        format!("last known {}{next}", line.text())
                    })
            }
            _ => None,
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reading {
    pub(crate) settle: Settle,
    /// `TARGET STATE [cause]`, bounded; equal lines are one observation.
    pub(crate) line: String,
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

/// Holds a settling PR reading pending, for a bounded period, while Actions has
/// reported no run for its head. A push's checks start after the push, so the
/// first reading of a new head can look settled before its workflows exist.
struct ActionsGrace {
    limit: Duration,
    /// The head last read and when it was first read.
    head: Option<(String, Instant)>,
    /// When the last reading was held, the moment the hold ends. The waiter
    /// must read again then: no event or timer of its own would wake it.
    held_until: Option<Instant>,
}
impl ActionsGrace {
    fn new(limit: Duration) -> Self {
        Self {
            limit,
            head: None,
            held_until: None,
        }
    }

    /// How long until the hold on the last reading ends; `None` when the last
    /// reading was not held.
    fn due_in(&self) -> Option<Duration> {
        self.held_until
            .map(|end| end.saturating_duration_since(Instant::now()))
    }

    /// Whether a held reading's grace period has ended and it needs a fresh read.
    fn is_due(&self) -> bool {
        self.due_in().is_some_and(|left| left.is_zero())
    }

    fn apply(&mut self, reading: Reading, label: &str, complete: bool) -> Reading {
        self.held_until = None;
        let Ok(mut line) = serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
        else {
            return reading;
        };
        let first_read = match &self.head {
            Some((head, at)) if *head == line.head => *at,
            _ => {
                let now = Instant::now();
                self.head = Some((line.head.clone(), now));
                now
            }
        };
        if first_read.elapsed() >= self.limit || !line.awaits_actions(complete) {
            return reading;
        }
        line.hold_for_actions();
        self.held_until = Some(first_read + self.limit);
        Reading {
            settle: Settle::Pending,
            line: format!(
                "{label} {}{} head={}",
                line.class(),
                line.cause_suffix(),
                watched_head(&line)
            ),
            data: json!(line),
        }
    }
}

/// A PR reading is only a verdict about the head being watched. While GitHub
/// reports a different head, keep the reading pending whatever its checks say.
fn gate_line(
    mut line: crate::pr::status::Line,
    watch_head: Option<&str>,
) -> crate::pr::status::Line {
    if let Some(head) = watch_head
        && line.awaiting.is_none()
        && line.head != head
    {
        line.await_head(head);
    }
    line
}

/// The head a PR reading is watching, always shown in its status line.
fn watched_head(line: &crate::pr::status::Line) -> &str {
    line.awaiting.as_deref().unwrap_or(&line.head)
}

/// The tip of a branch known locally, checked in the local repository only.
/// Both the checked-out branch and a remote-tracking branch count; a missing
/// repository or an unsafe branch name simply means no local tip is known.
fn local_branch_tip(cwd: &Path, branch: &str) -> Option<String> {
    let binary = provenant_os::git::git_executable()?;
    let refs = [
        provenant_confinement::repo::GitRefName::branch(branch).ok()?,
        provenant_confinement::repo::GitRefName::remote_tracking(format!("origin/{branch}"))
            .ok()?,
    ];
    let environment: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            !key.to_str()
                .is_some_and(|key| crate::fabric::GIT_REDIRECTS.contains(&key))
        })
        .chain([("GIT_TERMINAL_PROMPT".into(), "0".into())])
        .collect();
    let executor = SystemExecutor::new(1).ok()?;
    for reference in refs {
        let argv: Vec<OsString> = [
            binary.clone().into_os_string(),
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(reference.as_str()),
        ]
        .into();
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(10));
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 128 };
        options.stderr = StreamPolicy::Capture { limit: 128 };
        let Ok(out) = executor.execute(options) else {
            continue;
        };
        if out.termination != Termination::Exit(0) {
            continue;
        }
        let tip = String::from_utf8_lossy(&out.stdout.bytes).trim().to_owned();
        if provenant_core::GitOid::parse(&tip).is_ok() {
            return Some(tip);
        }
    }
    None
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

    match source {
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
                Ok(rows) => classify_rows(&label, &rows),
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            }
        }
        Source::Pr {
            number,
            repository,
            watch_head,
            complete,
        } => match repository {
            Ok(repository) => match crate::pr::status::wait_line_observed(
                edge.cwd,
                edge.roots.state.as_path(),
                repository,
                *number,
                &forge_request(deadline, cancel, pr_read_timeout),
                // Spec: docs/specs/cli.md#ci-observations — the PR path reads
                // through the one observer and its one sourced retry family.
                crate::pr::observer::retry(edge.roots).poll.value,
            ) {
                Ok(line) => {
                    let mut line = gate_line(line, watch_head.as_deref());
                    line.attach_failure_digest(
                        edge.roots.state.as_path(),
                        edge.cwd,
                        &forge_request(deadline, cancel, pr_read_timeout),
                    );
                    Reading {
                        settle: match line.wait_verdict(*complete) {
                            Some(true) => Settle::Succeeded,
                            Some(false) => Settle::Unsuccessful,
                            None => Settle::Pending,
                        },
                        line: format!(
                            "{label} {}{} head={}",
                            line.class(),
                            line.cause_suffix(),
                            watched_head(&line)
                        ),
                        data: json!(line),
                    }
                }
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            },
            Err(error) => Reading::unavailable(&label, error),
        },
    }
}

pub(crate) fn reading_pr_event(
    number: u64,
    watch_head: Option<&str>,
    complete: bool,
    detail: Value,
) -> Reading {
    let label = format!("#{number}");
    let status = &detail["status"];
    let malformed: Vec<_> = [
        "draft",
        "checks_passed",
        "checks_total",
        "checks_failed",
        "checks_pending",
        "failing_checks",
        "pending_checks",
        "merge",
        "review",
        "error",
    ]
    .into_iter()
    .filter(|field| {
        status.get(*field).is_some_and(|value| match *field {
            "draft" => !value.is_boolean(),
            "checks_passed" | "checks_total" | "checks_failed" | "checks_pending" => {
                !value.is_null()
                    && value
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .is_none()
            }
            "failing_checks" | "pending_checks" => !value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            "error" => !value.is_null() && !value.is_string(),
            _ => !value.is_string(),
        })
    })
    .collect();
    match serde_json::from_value::<crate::pr::status::Line>(status.clone()) {
        Ok(line) => {
            let line = gate_line(line, watch_head);
            let summary = format!(
                "{label} {}{} head={}",
                line.class(),
                line.cause_suffix(),
                watched_head(&line)
            );
            Reading {
                settle: match line.wait_verdict(complete) {
                    Some(true) => Settle::Succeeded,
                    Some(false) => Settle::Unsuccessful,
                    None => Settle::Pending,
                },
                line: if malformed.is_empty() {
                    summary
                } else {
                    format!(
                        "{summary} warning: invalid display fields {}",
                        malformed.join(",")
                    )
                },
                data: json!(line),
            }
        }
        Err(error) => Reading::unavailable(&label, &format!("invalid PR journal status: {error}")),
    }
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
        if let Ok(slot) = self.event_wake.lock()
            && let Some(wake) = slot.as_ref()
        {
            let _ = wake.wake();
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

fn graph_observation_paths(
    state: &provenant_core::StateRoot,
    project: provenant_core::ProjectId,
    graph: &str,
) -> Result<Vec<PathBuf>, String> {
    provenant_fabric_runtime::graph::FileJournal::observation_paths(state, project, graph)
        .map_err(|error| error.to_string())
}

pub(crate) fn observation_text(
    watcher: Option<&provenant_os::changes::watch::Watcher>,
    cadence: &LocalCadence,
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
            "paced reread {}; event source unavailable: {}",
            cadence.every(),
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
    cadence: LocalCadence,
    fallback: Option<String>,
    has_local_paths: bool,
    next_paced: Option<provenant_core::MonoTime>,
}
impl LocalObservation {
    pub(crate) fn subscribe(paths: &[PathBuf], cadence: LocalCadence) -> Self {
        if paths.is_empty() {
            return Self {
                watcher: None,
                cadence,
                fallback: Some("no local event targets".into()),
                has_local_paths: false,
                next_paced: None,
            };
        }
        match provenant_os::changes::watch::Watcher::subscribe(paths) {
            Ok(watcher) => Self {
                watcher: Some(watcher),
                cadence,
                fallback: None,
                has_local_paths: true,
                next_paced: None,
            },
            Err(error) => Self {
                watcher: None,
                cadence,
                fallback: Some(error.to_string()),
                has_local_paths: true,
                next_paced: None,
            },
        }
    }

    pub(crate) fn paced(cadence: LocalCadence, reason: impl Into<String>) -> Self {
        Self {
            watcher: None,
            cadence,
            fallback: Some(reason.into()),
            has_local_paths: true,
            next_paced: None,
        }
    }

    pub(crate) fn interval(&self) -> Duration {
        self.cadence.interval
    }

    pub(crate) fn report(&self, remote_pending: bool) -> String {
        if !self.has_local_paths {
            return if remote_pending {
                format!(
                    "peer statuses reread {}; no local event targets",
                    self.cadence.every()
                )
            } else {
                "no local event targets".into()
            };
        }
        observation_text_with_remote(
            self.watcher.as_ref(),
            &self.cadence,
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
            provenant_core::DurationMicros::from_micros(self.cadence.interval.as_micros() as u64);
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
    cadence: &LocalCadence,
    fallback: Option<&str>,
    remote_pending: bool,
) -> String {
    let mut text = observation_text(watcher, cadence, fallback);
    if remote_pending {
        text.push_str(&format!(
            "; peer/relay or unrepresented rows reread {} because the relay has no remote event subscription",
            cadence.every()
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
        cadence: &LocalCadence,
        fallback: Option<&str>,
    ) -> String {
        observation_text_with_remote(watcher, cadence, fallback, self.remote_source_seen)
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

/// Whether the wait is done: under `any`, once one target settled or needs
/// input; otherwise once every target has.
fn wait_done(latest: &[Option<Reading>], settled: &[usize], mode: WaitMode) -> bool {
    match mode {
        WaitMode::Any => !settled.is_empty(),
        WaitMode::All => latest
            .iter()
            .all(|reading| reading.as_ref().is_some_and(Reading::done)),
        WaitMode::FailFast => {
            latest
                .iter()
                .flatten()
                .any(|reading| reading.settle == Settle::Unsuccessful)
                || latest
                    .iter()
                    .all(|reading| reading.as_ref().is_some_and(Reading::done))
        }
    }
}

/// The parts of a reading that say what was observed, without its staleness.
fn observed(reading: &Reading) -> (Settle, &str, Value) {
    let mut data = reading.data.clone();
    if let Some(fields) = data.as_object_mut() {
        fields.remove("stale");
    }
    (reading.settle, reading.line.as_str(), data)
}

/// Fold one reading into the wait. A failed read keeps the last snapshot,
/// marked stale. `changed` sees each distinct observation once, and `settled`
/// records the order targets finished in.
fn absorb(
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    mode: WaitMode,
    mut update: Update,
    changed: &mut impl FnMut(usize, &Reading),
) {
    let index = update.target;
    if update.reading.data.is_null()
        && let Some(previous) = latest[index]
            .as_ref()
            .filter(|reading| reading.data.is_object())
    {
        update.reading.data = previous.data.clone();
        update.reading.data["stale"] = json!(true);
    }
    if latest[index]
        .as_ref()
        .is_some_and(|previous| observed(previous) == observed(&update.reading))
    {
        latest[index] = Some(update.reading);
        return;
    }
    let node_verdict = latest[index]
        .as_ref()
        .and_then(|previous| settlement_verdict(&new_settlements(Some(previous), &update.reading)));
    changed(index, &update.reading);
    if !update.reading.done()
        && let Some(verdict) = node_verdict
    {
        match mode {
            WaitMode::Any => update.reading.settle = verdict,
            WaitMode::FailFast if verdict == Settle::Unsuccessful => {
                update.reading.settle = verdict;
            }
            WaitMode::All | WaitMode::FailFast => {}
        }
    }
    if update.reading.done() && !settled.contains(&index) {
        settled.push(index);
    }
    latest[index] = Some(update.reading);
}

/// Fold source readings until the wait is done. Returns false when the
/// deadline passes first, leaving unsettled readings marked stale.
fn collect(
    receiver: &mpsc::Receiver<Update>,
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    deadline: Instant,
    mode: WaitMode,
    changed: &mut impl FnMut(usize, &Reading),
) -> bool {
    loop {
        if wait_done(latest, settled, mode) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(update) = receiver.recv_timeout(left) else {
            for reading in latest.iter_mut().flatten() {
                if !reading.done() && reading.data.is_object() {
                    reading.data["stale"] = json!(true);
                }
            }
            return false;
        };
        absorb(latest, settled, mode, update, changed);
    }
}

/// Expiry ends blocking observation; then each unsettled local graph target
/// gets one final nonblocking snapshot of its local store, so a settlement
/// after the last sample (event or paced tick) is reported, not a timeout.
/// Pull requests and peer or relayed rows keep their last reading: remote
/// reads use only the original budget. Returns whether the wait is now done.
fn final_snapshot(
    sources: &[Source],
    latest: &mut [Option<Reading>],
    settled: &mut Vec<usize>,
    mode: WaitMode,
    host: Option<&provenant_core::HostId>,
    mut read_local: impl FnMut(&provenant_cli::Target) -> Option<Reading>,
    changed: &mut impl FnMut(usize, &Reading),
) -> bool {
    for (index, source) in sources.iter().enumerate() {
        if wait_done(latest, settled, mode) {
            break;
        }
        let Source::Graph(target) = source else {
            continue;
        };
        let local = latest[index].as_ref().is_none_or(|reading| {
            !reading.done()
                && (!reading.data.is_object() || reading_locally_represented(reading, host))
        });
        if !local {
            continue;
        }
        if let Some(reading) = read_local(target) {
            absorb(
                latest,
                settled,
                mode,
                Update {
                    target: index,
                    reading,
                },
                changed,
            );
        }
    }
    wait_done(latest, settled, mode)
}

/// One local graph target's final snapshot; `None` when the local store cannot
/// answer without blocking.
fn final_read(
    target: &provenant_cli::Target,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<provenant_cli::fabric::Context>,
) -> Option<Reading> {
    provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
        crate::fabric::final_status_rows(target, edge, session)
    })
    .ok()
    .map(|rows| classify_rows(&target.to_string(), &rows))
}

/// One forge request for a pull request read: the wait's absolute deadline,
/// capped by `domains.wait.pr_read_timeout_seconds`, and the waiter's
/// cancellation, so a target that settles elsewhere interrupts the transport.
fn forge_request(
    deadline: Instant,
    cancel: &CancelToken,
    pr_read_timeout: Duration,
) -> provenant_config::ports::Request {
    provenant_config::ports::Request::new(deadline, cancel.clone()).capped(pr_read_timeout)
}

/// The local paced-reread cadence for owner-side waits, resolved through the
/// one wait settings policy. An unreadable or malformed configuration warns and
/// uses the compiled defaults rather than refusing the wait.
pub(crate) fn local_cadence(
    roots: &provenant_core::Roots,
) -> Result<LocalCadence, provenant_fabric_runtime::coordination::facade::FacadeError> {
    use provenant_fabric_runtime::coordination::facade::{FacadeError, InvalidInputDetail};
    let (settings, origin) = provenant_cli::wait::Settings::resolve(roots).map_err(|error| {
        FacadeError::invalid(InvalidInputDetail::Configuration(error.to_string()))
    })?;
    Ok(origin.local_cadence(&settings))
}

/// How the wait ended: a timeout, else the worst of the targets that decided
/// it. Under `any` only the first target to finish decides; fail-fast returns
/// on the first unsuccessful target, otherwise it reports all final results.
fn outcome(latest: &[Option<Reading>], settled: &[usize], done: bool, mode: WaitMode) -> Outcome {
    if !done {
        return Outcome::Timeout;
    }
    let deciding: Vec<_> = match mode {
        WaitMode::Any => settled.iter().copied().take(1).collect(),
        WaitMode::All => settled.to_vec(),
        WaitMode::FailFast => {
            let failures: Vec<_> = settled
                .iter()
                .copied()
                .filter(|index| {
                    latest[*index]
                        .as_ref()
                        .is_some_and(|reading| reading.settle == Settle::Unsuccessful)
                })
                .take(1)
                .collect();
            if failures.is_empty() {
                settled.to_vec()
            } else {
                failures
            }
        }
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

/// The settle verdict for a child row from the owner relay. A settled node is
/// either `succeeded` or one of the terminal unsuccessful states; only a node
/// the relay still reports as running stays pending.
fn child_settle(child: &Value) -> Settle {
    if child["settled"] != true {
        return Settle::Pending;
    }
    if child["state"] == "succeeded" {
        Settle::Succeeded
    } else {
        Settle::Unsuccessful
    }
}

/// The verdict for a child wait is drawn from exactly the children the mode
/// decided on: the first settled child under `--any`, the first failed child
/// under `--fail-fast`, and every child otherwise. A child still running when
/// the wait ends early is not a failure.
fn child_wait_outcome(children: &[Value], mode: WaitMode, settled_order: &[usize]) -> Outcome {
    let verdicts: Vec<Settle> = match mode {
        WaitMode::Any => settled_order
            .iter()
            .take(1)
            .filter_map(|&index| children.get(index).map(child_settle))
            .collect(),
        WaitMode::FailFast => match settled_order.iter().copied().find(|&index| {
            children
                .get(index)
                .is_some_and(|child| child_settle(child) == Settle::Unsuccessful)
        }) {
            Some(index) => vec![child_settle(&children[index])],
            None => children.iter().map(child_settle).collect(),
        },
        WaitMode::All => children.iter().map(child_settle).collect(),
    };
    if verdicts.contains(&Settle::Unsuccessful) {
        Outcome::Failed
    } else {
        Outcome::Ok
    }
}

/// A wait inside a lane that names no targets observes the parent's children
/// through the owner relay. Those terminal child states are the wait's targets:
/// emit the same settlement events and terminal envelope a named-target wait
/// emits. Discarding them would read a failed child as success and leave
/// `wait --json` with no terminal result at all. Settlement events are streamed
/// by the polling callback so `--follow` sees transitions as they happen; this
/// builds only the terminal envelope and its final rows.
fn emit_child_wait(
    children: &[Value],
    cadence: &LocalCadence,
    mode: WaitMode,
    globals: &Globals,
    settled_order: &[usize],
    write_error: Option<std::io::Error>,
) -> u8 {
    let observation = provenant_cli::fabric::graph_relay::child_wait_observation(cadence);
    let mut human = mode_heading(mode);
    let mut rows = Vec::new();
    let mut summary_lines = Vec::new();
    for child in children {
        let label = child["node"].as_str().unwrap_or("child").to_owned();
        let settle = child_settle(child);
        let line = format!("{label} {}", settle_phrase(settle, true));
        human.push_str(&line);
        human.push('\n');
        summary_lines.push(line.clone());
        rows.push(json!({
            "target": label,
            "settle": settle,
            "line": line,
            "data": child,
        }));
    }
    if children.is_empty() {
        human.push_str("nothing to wait for: no child work\n");
    }
    let states = if summary_lines.is_empty() {
        "nothing to wait for".to_owned()
    } else {
        summary_lines.join(", ")
    };
    human.push_str(&format!("next: {states}; provenant status\n"));
    if let Some(error) = write_error {
        return crate::fail(
            "wait",
            Outcome::Error,
            "IO",
            &error.to_string(),
            "check the reported output path and its permissions, then rerun `provenant wait`",
            globals,
        );
    }
    let outcome = child_wait_outcome(children, mode, settled_order);
    let summary = provenant_cli::Summary::new(
        mode_heading(mode).trim_end().to_owned(),
        summary_lines,
        Some(format!("{states}; next: provenant status")),
    );
    crate::emit_summarised(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            terminal_data(mode, rows, false, Some(&observation)),
        ),
        globals,
        &human,
        true,
        Some(&summary),
    )
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
            "run `provenant status` to see the work, then rerun `provenant wait` to resume observing",
            globals,
        )
    } else if Instant::now() >= deadline {
        crate::fail(
            "wait",
            Outcome::Timeout,
            "TIMEOUT",
            "wait timed out; the work continues",
            "rerun `provenant wait` with a longer `--timeout`, or check `provenant status`; the work continues",
            globals,
        )
    } else {
        crate::fabric::command_fail("wait", error, globals)
    }
}

pub(crate) fn run(command: provenant_cli::Wait, globals: &Globals) -> u8 {
    use crate::roots::{Product, Workspace};
    let mode = WaitMode::from_command(&command);
    let resolved = match crate::roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail(
                "wait",
                Outcome::Error,
                "ROOT",
                &error.to_string(),
                "run from a project checkout or pass `--project PATH`",
                globals,
            );
        }
    };
    let (settings, origin) = match provenant_cli::wait::Settings::resolve(&resolved.roots) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail(
                "wait",
                Outcome::Error,
                "CONFIG",
                &error.to_string(),
                "run `provenant check config --json` and correct the reported wait setting",
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
    // A `--pr --until complete` wait must outlast a normal CI run, so it uses
    // its own configured default budget. An explicit --timeout still wins and
    // every budget shares the same maximum.
    let complete = matches!(command.until, Some(provenant_cli::PrWaitUntil::Complete));
    let budget = if complete {
        settings.complete_timeout(command.timeout, &origin)
    } else {
        settings.timeout(command.timeout, &origin)
    };
    let deadline = Instant::now() + Duration::from_secs(budget);
    // Preflight reads tolerate a momentarily busy store instead of failing a
    // wait the instant another writer holds a lock. Capped by the wait's own
    // deadline, and configurable through `domains.wait.preflight_busy_timeout_ms`.
    let preflight_busy = Duration::from_millis(settings.preflight_busy_timeout_ms)
        .min(deadline.saturating_duration_since(Instant::now()));
    let stop = Stop::default();
    let cadence = origin.local_cadence(&settings);
    let interval = cadence.interval;
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
    let local_host = project.as_ref().ok().map(|(_, host)| *host);
    let project_id = project.as_ref().ok().map(|(id, _)| *id);
    let project_error = project.as_ref().err().cloned();
    let mut pending_index_error = None;
    let mut observation_path_error = None;
    let event_paths = match project {
        Ok((project, _)) if !command.targets.is_empty() => {
            let paths = command
                .targets
                .iter()
                .map(|target| graph_observation_paths(&resolved.roots.state, project, &target.name))
                .collect::<Result<Vec<_>, _>>();
            match paths {
                Ok(paths) => paths.into_iter().flatten().collect(),
                Err(error) => {
                    observation_path_error =
                        Some(format!("cannot resolve graph observation paths: {error}"));
                    Vec::new()
                }
            }
        }
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
                    match names
                        .iter()
                        .map(|name| graph_observation_paths(&resolved.roots.state, project, name))
                        .collect::<Result<Vec<_>, _>>()
                    {
                        Ok(paths) => paths.into_iter().flatten().collect(),
                        Err(error) => {
                            observation_path_error =
                                Some(format!("cannot resolve graph observation paths: {error}"));
                            Vec::new()
                        }
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
    // Capture the shared journal cursor before initial reconciliation. Only
    // values cross the worker boundary; SQLite connections remain thread local.
    let graph_subscription = project.as_ref().ok().and_then(|(project, _)| {
        crate::event_delivery::Journal::tip(&resolved.roots, *project)
            .ok()
            .map(|cursor| (resolved.roots.clone(), *project, cursor))
    });
    let event_paths = if graph_subscription.is_some() && !event_paths.is_empty() {
        vec![
            resolved
                .roots
                .state
                .as_path()
                .join(provenant_core::changes::FILE_NAME),
        ]
    } else {
        event_paths
    };
    let setup_fallback = project_error
        .or(pending_index_error)
        .or(observation_path_error);
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
        &cadence,
        fallback_reason.as_deref(),
        false,
    )));
    let mut validation = std::collections::BTreeMap::new();
    if let Err(error) = provenant_sqlite::with_busy_timeout(preflight_busy, || {
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
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: [TARGET_REQUIRED] wait inside a lane named no targets; waiting on this lane's children through the owner relay"
            );
            // The local project journal is unavailable inside a lane and the
            // project-wide unsettled set would include this lane's own pending
            // graph, so an all-mode wait could never settle. Discover the
            // lane's children through the relay and wait for those instead,
            // ending on and reporting exactly the mode the caller selected.
            let within = deadline.saturating_duration_since(Instant::now());
            let child_mode = match mode {
                WaitMode::Any => provenant_cli::fabric::graph_relay::ChildWaitMode::Any,
                WaitMode::FailFast => provenant_cli::fabric::graph_relay::ChildWaitMode::FailFast,
                WaitMode::All => provenant_cli::fabric::graph_relay::ChildWaitMode::All,
            };
            let mut announced: std::collections::HashMap<String, Settle> = Default::default();
            let mut settled_order: Vec<usize> = Vec::new();
            let mut write_error = None;
            let mut observe = |children: &[Value]| {
                for (index, child) in children.iter().enumerate() {
                    let settle = child_settle(child);
                    // Without --follow only settlements are events; with it,
                    // every newly observed state is a transition, as for named
                    // targets.
                    if !command.follow && settle == Settle::Pending {
                        continue;
                    }
                    let label = child["node"].as_str().unwrap_or("child").to_owned();
                    if announced.get(&label) == Some(&settle) {
                        continue;
                    }
                    announced.insert(label.clone(), settle);
                    if settle != Settle::Pending && !settled_order.contains(&index) {
                        settled_order.push(index);
                    }
                    if write_error.is_some() {
                        continue;
                    }
                    let line = format!("{label} {}", settle_phrase(settle, true));
                    let event = if settle == Settle::Pending {
                        "transition"
                    } else {
                        "settled"
                    };
                    let event = if globals.json {
                        json!({"event":event,"target":label,"settle":settle,"line":line})
                            .to_string()
                    } else {
                        line
                    };
                    write_error = write_line(&event).err();
                }
            };
            let waited = provenant_cli::fabric::graph_relay::wait_for_children(
                &stop.cancel,
                within,
                cadence.interval,
                child_mode,
                &mut observe,
            );
            drop(observe);
            return match waited {
                Ok(settled_children) => {
                    let children = settled_children["children"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    emit_child_wait(
                        &children,
                        &cadence,
                        mode,
                        globals,
                        &settled_order,
                        write_error,
                    )
                }
                Err(error) => match error.code() {
                    "cancelled" => crate::fail(
                        "wait",
                        Outcome::Cancelled,
                        "CANCELLED",
                        "wait cancelled; the work continues",
                        "run `provenant status` to see the children, then rerun `provenant wait`",
                        globals,
                    ),
                    "timeout" => crate::fail(
                        "wait",
                        Outcome::Timeout,
                        "TIMEOUT",
                        "wait timed out; the work continues",
                        "rerun `provenant wait` with a longer `--timeout` or check `provenant status`",
                        globals,
                    ),
                    _ => crate::fail(
                        "wait",
                        Outcome::Error,
                        "CHILD_WAIT",
                        &format!("cannot wait on this lane's children: {error}"),
                        "check the lane relay and rerun `provenant wait`",
                        globals,
                    ),
                },
            };
        }
        match provenant_sqlite::with_busy_timeout(preflight_busy, || {
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
    let actions_grace = Duration::from_secs(settings.actions_grace_seconds);
    if sources.len() + prs.len() > settings.max_targets as usize {
        return crate::fail(
            "wait",
            Outcome::Invalid,
            "TOO_MANY_TARGETS",
            &format!("wait takes at most {} targets", settings.max_targets),
            "remove targets or raise `domains.wait.max_targets`, then rerun `provenant wait`",
            globals,
        );
    }
    if !prs.is_empty() {
        let request = forge_request(deadline, &stop.cancel, pr_read_timeout);
        let repository =
            crate::pr::status::wait_repository(edge.roots.state.as_path(), edge.cwd, &request)
                .map_err(|error| error.to_string());
        sources.extend(prs.into_iter().map(|number| {
            let watch_head = command.head.clone().or_else(|| {
                let repository = repository.as_ref().ok()?;
                let reported = crate::pr::status::wait_head(
                    edge.roots.state.as_path(),
                    edge.cwd,
                    repository,
                    number,
                    &request,
                )
                .ok()?;
                let local = local_branch_tip(edge.cwd, &reported.branch)?;
                (local != reported.sha).then(|| {
                    provenant_records::warning::warn(
                        provenant_records::warning::Warning::new(
                            provenant_records::warning::BRANCH_BEHIND_BASE,
                            format!(
                                "#{number}: local tip {local} differs from GitHub head {}; waiting for the local tip",
                                reported.sha
                            ),
                        )
                        .subject(reported.branch.clone()),
                    );
                    local
                })
            });
            Source::Pr {
                number,
                repository: repository.clone(),
                watch_head,
                complete,
            }
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
            let cadence = &cadence;
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
                    cadence,
                    fallback_reason.as_deref(),
                );
            }
            scope.spawn(move || {
                let mut journal = graph_subscription.and_then(|(roots, project, cursor)|
                    crate::event_delivery::Journal::resume_graphs(roots, project, cursor).ok());
                let mut last_graph_reconcile = Instant::now();
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
                            cadence,
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
                        let wait_for = left.min(until_remote).min(Duration::from_secs(60).saturating_sub(last_graph_reconcile.elapsed()));
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
                                        cadence,
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
                    let gap_due = last_graph_reconcile.elapsed() >= Duration::from_secs(60);
                    if let Some(source) = journal.as_mut() {
                        match source.drain_graphs() {
                            Ok(graphs) if !gap_due => reads.retain(|index| {
                                remote_indices.contains(index) || graph_sources.iter().any(|(candidate, source)|
                                    candidate == index && matches!(source, Source::Graph(target) if graphs.contains(&target.name)))
                            }),
                            Ok(_) => { reads = pending.clone(); last_graph_reconcile = Instant::now(); }
                            Err(_) => { journal = None; reads = pending.clone(); last_graph_reconcile = Instant::now(); }
                        }
                    } else if gap_due {
                        reads = pending.clone();
                        last_graph_reconcile = Instant::now();
                    }
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
                                cadence,
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
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let interval = Duration::from_secs(60);
            scope.spawn(move || {
                let mut session = None;
                let (watch_head, complete) = match source {
                    Source::Pr {
                        watch_head,
                        complete,
                        ..
                    } => (watch_head.as_deref(), *complete),
                    _ => (None, false),
                };
                // Each subscription owns its replay cursor. No target can
                // consume another target's events from a shared cursor.
                let setup =
                    (|| -> Result<crate::pr::observer::journal::PrJournal, Box<dyn std::error::Error>> {
                        // The observer module owns both transports: this journal
                        // supplies a current PR observation when one exists, and
                        // `read` falls back to the forge through the observer.
                        let journal = crate::pr::observer::journal::PrJournal::open(
                            edge.roots.state.as_path(),
                            edge.cwd,
                        )?;
                        if let Source::Pr {
                            number,
                            repository: Ok(repository),
                            ..
                        } = source
                        {
                            journal.register(repository, *number, None)?;
                        }
                        Ok(journal)
                    })();
                let mut event_reader = match setup {
                    Ok(journal) => Some(journal),
                    Err(error) => {
                        provenant_records::warning::warn(provenant_records::warning::Warning::new(
                            provenant_records::warning::PR_JOURNAL_UNAVAILABLE,
                            format!("PR journal unavailable; reconciling after gaps: {error}"),
                        ));
                        None
                    }
                };
                let mut grace = ActionsGrace::new(actions_grace);
                let initial = grace.apply(
                    read(
                        source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                    ),
                    &source.label(),
                    complete,
                );
                let done = initial.done();
                if sender
                    .send(Update {
                        target: index,
                        reading: initial,
                    })
                    .is_err()
                    || done
                {
                    return;
                }
                let mut last_reconcile = Instant::now();
                loop {
                    if stop.cancel.is_cancelled()
                        || deadline.saturating_duration_since(Instant::now()).is_zero()
                    {
                        break;
                    }
                    let reading = match (event_reader.as_mut(), source) {
                        (
                            Some(journal),
                            Source::Pr {
                                number,
                                repository: Ok(repository),
                                ..
                            },
                        ) => match journal.next(
                            repository,
                            *number,
                            Duration::from_secs(60)
                                .saturating_sub(last_reconcile.elapsed())
                                .min(deadline.saturating_duration_since(Instant::now()))
                                .min(grace.due_in().unwrap_or(Duration::MAX)),
                            &stop.cancel,
                        ) {
                            Ok(Some(detail)) => {
                                last_reconcile = Instant::now();
                                if detail["status"].is_object() {
                                    let event =
                                        reading_pr_event(*number, watch_head, complete, detail);
                                    if complete || event.settle == Settle::Succeeded {
                                        // Journal records are bounded and omit Actions runs.
                                        // Reconcile through the observer before settling.
                                        read(
                                            source,
                                            edge,
                                            &mut session,
                                            deadline,
                                            pr_read_timeout,
                                            &stop.cancel,
                                        )
                                    } else {
                                        event
                                    }
                                } else {
                                    // Webhook records carry identifiers only;
                                    // fetch the authoritative PR status now.
                                    read(
                                        source,
                                        edge,
                                        &mut session,
                                        deadline,
                                        pr_read_timeout,
                                        &stop.cancel,
                                    )
                                }
                            }
                            Ok(None)
                                if last_reconcile.elapsed() >= Duration::from_secs(60)
                                    || grace.is_due() =>
                            {
                                last_reconcile = Instant::now();
                                // A held reading whose grace period ended needs a
                                // fresh read even when the journal is current:
                                // nothing else would settle it.
                                if !grace.is_due()
                                    && journal
                                        .source_current(repository, *number, None)
                                        .unwrap_or(false)
                                {
                                    continue;
                                }
                                read(
                                    source,
                                    edge,
                                    &mut session,
                                    deadline,
                                    pr_read_timeout,
                                    &stop.cancel,
                                )
                            }
                            Ok(None) => continue,
                            Err(error) => {
                                event_reader = None;
                                Reading::unavailable(
                                    &source.label(),
                                    &format!("PR event journal failed: {error}"),
                                )
                            }
                        },
                        _ => {
                            if !stop.pause(
                                interval
                                    .saturating_sub(last_reconcile.elapsed())
                                    .min(deadline.saturating_duration_since(Instant::now()))
                                    .min(grace.due_in().unwrap_or(Duration::MAX)),
                            ) {
                                break;
                            }
                            last_reconcile = Instant::now();
                            read(
                                source,
                                edge,
                                &mut session,
                                deadline,
                                pr_read_timeout,
                                &stop.cancel,
                            )
                        }
                    };
                    let reading = grace.apply(reading, &source.label(), complete);
                    let finished = reading.done();
                    if sender
                        .send(Update {
                            target: index,
                            reading,
                        })
                        .is_err()
                        || finished
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
            mode,
            &mut transition,
        );
        stop.stop();
        drop(receiver);
        done
    });
    let mut final_session = None;
    let done = done
        || final_snapshot(
            &sources,
            &mut latest,
            &mut settled,
            mode,
            local_host.as_ref(),
            |target| final_read(target, &edge, &mut final_session),
            &mut transition,
        );
    if let Some(error) = write_error {
        return crate::fail(
            "wait",
            Outcome::Error,
            "IO",
            &error.to_string(),
            "check the reported output path and its permissions, then rerun `provenant wait`",
            globals,
        );
    }
    let outcome = outcome(&latest, &settled, done, mode);
    let mut human = mode_heading(mode);
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
            if let (Source::Pr { complete: true, .. }, Some(reading)) = (source, reading)
                && let Ok(status) =
                    serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
            {
                for failure in status.failure_lines() {
                    human.push_str(&failure);
                    human.push('\n');
                }
                for digest in status.digest_rows() {
                    human.push_str(&digest);
                    human.push('\n');
                }
            }
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
        for detail in pr_timeout_details(&sources, &latest) {
            human.push_str(&detail);
            human.push('\n');
        }
        human.push_str("wait timed out; the work continues\n");
    }
    let observation = observation
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|_| "event source status unavailable".into());
    // The observation method is diagnostic detail, not the answer a chair
    // reads: it belongs to --full and to the JSON envelope, never to the last
    // line of default human output.
    if globals.full && !graph_indices.is_empty() {
        human.push_str(&format!("local observation: {observation}\n"));
    }
    // A superseded name warns on stderr; next_command points at the newer handle.
    let mut superseding = Vec::new();
    if let Some(project_id) = project_id {
        for target in &command.targets {
            if let Some(newest) =
                crate::fabric::superseding_handle(&resolved.roots.state, project_id, &target.name)
            {
                crate::fabric::superseded_notice(&target.name, &newest, "wait");
                superseding.push(newest);
            }
        }
    }
    human.push_str(&final_line(&sources, &latest, done, &superseding));
    // Compact output keeps an owner summary, not a head-only truncation, so
    // the settle states and the next command stay on its last line.
    let mut summary = provenant_cli::Summary::new(
        mode_heading(mode).trim_end().to_owned(),
        rows.iter()
            .map(|row| row["line"].as_str().unwrap_or("").to_owned())
            .collect(),
        Some(format!(
            "{}; {}",
            settle_states(&sources, &latest, done),
            next_command(&sources, &latest, &superseding)
        )),
    );
    // Completion reports must retain every failing check and its link in compact output.
    for (source, reading) in sources.iter().zip(&latest) {
        if let (Source::Pr { complete: true, .. }, Some(reading)) = (source, reading)
            && let Ok(status) =
                serde_json::from_value::<crate::pr::status::Line>(reading.data.clone())
        {
            summary.lines.extend(status.failure_lines());
            summary.lines.extend(status.digest_rows());
        }
    }
    crate::emit_summarised(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            terminal_data(
                mode,
                rows,
                !done,
                (!graph_indices.is_empty()).then_some(observation.as_str()),
            ),
        ),
        globals,
        &human,
        true,
        Some(&summary),
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
    #[test]
    fn pr_journal_display_fields_degrade_without_unavailable() {
        for value in [
            None,
            Some(serde_json::Value::Null),
            Some(serde_json::json!([])),
        ] {
            let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc",
                "checks_total":1,"checks_failed":0,"checks_pending":0,"review":"APPROVED"});
            if let Some(value) = value {
                status["failing_checks"] = value;
            }
            let reading =
                super::reading_pr_event(7, None, false, serde_json::json!({"status":status}));
            assert_eq!(reading.settle, super::Settle::Succeeded);
            assert!(!reading.line.contains("unavailable"));
        }
        for field in [
            "draft",
            "checks_passed",
            "checks_total",
            "failing_checks",
            "pending_checks",
            "merge",
        ] {
            let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
            status[field] = serde_json::json!({"invalid":true});
            let reading =
                super::reading_pr_event(7, None, false, serde_json::json!({"status":status}));
            assert!(!reading.line.contains("unavailable"), "{field}");
            assert!(reading.line.contains("warning"), "{field}");
            assert_eq!(reading.settle, super::Settle::Pending, "{field}");
        }
        // These fields affect the verdict. Malformed values fail closed as an
        // unavailable, still-pending observation instead of defaulting to a
        // successful or terminal classification.
        for field in ["checks_failed", "checks_pending", "review", "error"] {
            let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
            status[field] = serde_json::json!({"invalid":true});
            let reading =
                super::reading_pr_event(7, None, false, serde_json::json!({"status":status}));
            assert_eq!(reading.settle, super::Settle::Pending, "{field}");
            assert!(reading.line.contains("unavailable"), "{field}");
        }
        for field in ["number", "state", "head"] {
            let mut status = serde_json::json!({"number":7,"state":"OPEN","head":"abc"});
            status.as_object_mut().unwrap().remove(field);
            assert!(
                super::reading_pr_event(7, None, false, serde_json::json!({"status":status}))
                    .line
                    .contains("unavailable")
            );
        }
    }

    use super::*;
    #[allow(unused_imports)] // Also compiled by the harness=false worktree driver.
    use provenant_core::Clock as _;

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
        let host = provenant_core::HostId::parse(&"a".repeat(32)).expect("fixture host");
        let completed_peer = Reading {
            settle: Settle::Succeeded,
            line: "peer settled".into(),
            data: json!({"rows":[{"kind":"attempt","attempt_id":format!("{}/2", "b".repeat(32))}]}),
        };
        let mut history = ObservationHistory::default();
        history.record(&completed_peer, Some(&host));

        let final_observation = history.render(
            None,
            &LocalCadence::compiled(Duration::from_millis(500)),
            Some("event source unavailable: fixture parent missing"),
        );
        assert!(completed_peer.done());
        assert!(final_observation.contains("peer/relay or unrepresented rows reread every 500 ms"));
    }

    #[test]
    fn local_observation_reports_the_fallback_source_and_reason() {
        let observation = LocalObservation::paced(
            LocalCadence::compiled(Duration::from_millis(40)),
            "fixture has no native event backend",
        );
        let report = observation.report(false);
        assert!(
            report.contains(provenant_fabric_runtime::coordination::LOCAL_OBSERVATION_INTERVAL_KEY)
        );
        assert!(report.contains("fixture has no native event backend"));
    }

    #[test]
    fn repeated_short_waits_eventually_reach_one_paced_fallback_tick() {
        let interval = Duration::from_millis(40);
        let mut observation = LocalObservation::paced(
            LocalCadence::compiled(interval),
            "fixture event source unavailable",
        );
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
        let mut observation = LocalObservation::subscribe(
            std::slice::from_ref(&log),
            LocalCadence::compiled(Duration::from_secs(1)),
        );
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
            LocalCadence::compiled(Duration::from_secs(1)),
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
                WaitMode::All,
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
    fn a_target_settling_after_the_last_sample_is_reported_settled_at_expiry() {
        // Expiry is modelled without a clock: the deadline has passed and the
        // workers are gone, leaving only the last paced sample, which saw the
        // graph pending. Its local store has settled since.
        let host = provenant_core::HostId::parse(&"a".repeat(32)).expect("fixture host");
        let graph = |state: &str, outcome: &str| {
            classify_rows(
                "fix",
                &[json!({"kind":"graph","state":state,"outcome":outcome})],
            )
        };
        let pull = Source::Pr {
            number: 7,
            repository: Ok("owner/repo".into()),
            watch_head: None,
            complete: false,
        };
        for (with_pull, expected) in [(false, Outcome::Ok), (true, Outcome::Timeout)] {
            let mut sources = vec![Source::Graph("fix".parse().expect("target"))];
            if with_pull {
                sources.push(pull.clone());
            }
            let (sender, receiver) = mpsc::channel();
            sender
                .send(Update {
                    target: 0,
                    reading: graph("pending", ""),
                })
                .expect("queue the last sample");
            drop(sender);
            let mut latest = vec![None; sources.len()];
            if with_pull {
                latest[1] = Some(reading(Settle::Pending));
            }
            let mut settled = Vec::new();
            let done = collect(
                &receiver,
                &mut latest,
                &mut settled,
                Instant::now(),
                WaitMode::All,
                &mut |_, _| {},
            );
            assert!(!done, "the blocking phase ends unsatisfied");
            let mut reads = Vec::new();
            let done = final_snapshot(
                &sources,
                &mut latest,
                &mut settled,
                WaitMode::All,
                Some(&host),
                |target| {
                    reads.push(target.to_string());
                    Some(graph("complete", "succeeded"))
                },
                &mut |_, _| {},
            );
            assert_eq!(reads, ["fix"], "only the local store is read after expiry");
            assert_eq!(
                latest[0].as_ref().map(|r| r.settle),
                Some(Settle::Succeeded)
            );
            assert_eq!(
                latest[0].as_ref().map(|r| r.data["stale"].is_null()),
                Some(true)
            );
            assert_eq!(outcome(&latest, &settled, done, WaitMode::All), expected);
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

    #[test]
    fn final_line_states_every_settle_and_the_next_command() {
        use Settle::*;
        let sources = vec![
            Source::Graph("fixed".parse().expect("target")),
            Source::Graph("broken".parse().expect("target")),
            Source::Graph("input".parse().expect("target")),
            Source::Graph("slow".parse().expect("target")),
        ];
        let mixed = vec![
            Some(reading(Succeeded)),
            Some(reading(Unsuccessful)),
            Some(reading(NeedsInput)),
            Some(reading(Pending)),
        ];
        assert_eq!(
            final_line(&sources, &mixed, false, &[]),
            "next: fixed settled ok, broken failed, input needs input, slow still running at timeout; provenant wait slow\n"
        );
        let settled = vec![
            Some(reading(Succeeded)),
            Some(reading(Unsuccessful)),
            Some(reading(NeedsInput)),
            Some(reading(Succeeded)),
        ];
        assert_eq!(
            final_line(&sources, &settled, true, &[]),
            "next: fixed settled ok, broken failed, input needs input, slow settled ok; provenant status fixed broken input slow\n"
        );
        assert_eq!(
            final_line(&[], &[], true, &[]),
            "next: nothing to wait for; provenant status\n"
        );
        // A superseded target keeps its states; the next command names the newer handle.
        assert_eq!(
            final_line(&sources, &settled, true, &["slow-2".to_owned()]),
            "next: fixed settled ok, broken failed, input needs input, slow settled ok; provenant wait slow-2\n"
        );
    }

    fn reading(settle: Settle) -> Reading {
        Reading {
            settle,
            line: format!("{settle:?}"),
            data: Value::Null,
        }
    }

    fn wait_on(updates: &[(usize, Settle)], count: usize, mode: WaitMode) -> (Outcome, Vec<usize>) {
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
            mode,
            &mut |_, _| {},
        );
        (outcome(&latest, &settled, done, mode), settled)
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
            WaitMode::Any,
        );
        assert_eq!(settled, [1], "the later settlement was never read");
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(
            wait_on(&[(0, Pending), (1, NeedsInput)], 2, WaitMode::Any).0,
            Outcome::Pending
        );
    }

    #[test]
    fn all_waits_for_every_target_and_a_timeout_is_124() {
        use Settle::*;
        let (outcome, _) = wait_on(&[(0, Succeeded), (1, Pending)], 2, WaitMode::All);
        assert_eq!(provenant_cli::exit_status(&outcome), 124);
        let (outcome, _) = wait_on(
            &[(0, Succeeded), (1, Pending), (1, Succeeded)],
            2,
            WaitMode::All,
        );
        assert_eq!(provenant_cli::exit_status(&outcome), 0);
        let (outcome, _) = wait_on(&[(0, NeedsInput), (1, Unsuccessful)], 2, WaitMode::All);
        assert_eq!(provenant_cli::exit_status(&outcome), 1);
        let (outcome, settled) = wait_on(
            &[(1, Unsuccessful), (0, Pending), (0, Succeeded)],
            2,
            WaitMode::All,
        );
        assert_eq!(
            settled,
            [1, 0],
            "all mode reads the remaining target after failure"
        );
        assert_eq!(outcome, Outcome::Failed);
        assert_eq!(
            provenant_cli::exit_status(&wait_on(&[], 1, WaitMode::Any).0),
            124
        );
    }

    #[test]
    fn fail_fast_waits_for_successes_but_returns_on_first_failure() {
        use Settle::*;
        let (outcome, settled) = wait_on(
            &[
                (0, Pending),
                (1, Pending),
                (1, Unsuccessful),
                (0, Succeeded),
            ],
            2,
            WaitMode::FailFast,
        );
        assert_eq!(settled, [1]);
        assert_eq!(outcome, Outcome::Failed);
        let (outcome, settled) = wait_on(
            &[(0, Pending), (1, Pending), (1, Succeeded), (0, Succeeded)],
            2,
            WaitMode::FailFast,
        );
        assert_eq!(settled, [1, 0]);
        assert_eq!(outcome, Outcome::Ok);
    }

    #[test]
    fn a_failed_child_is_never_reported_as_a_successful_wait() {
        use serde_json::json;
        // The relay's terminal child rows are the wait's targets. A `failed`
        // or otherwise unsuccessful settled node must make the wait fail, not
        // silently return 0 with no result.
        let child = |node: &str, state: &str| json!({"node":node,"state":state,"settled":state != "running"});
        assert_eq!(
            super::child_settle(&child("a", "succeeded")),
            Settle::Succeeded
        );
        for state in ["failed", "blocked", "skipped"] {
            assert_eq!(
                super::child_settle(&child("a", state)),
                Settle::Unsuccessful,
                "state={state}"
            );
        }
        assert_eq!(super::child_settle(&child("a", "running")), Settle::Pending);
    }

    #[test]
    fn a_child_wait_takes_its_verdict_from_the_selected_mode() {
        let child = |node: &str, state: &str| json!({"node":node,"state":state,"settled":state != "running"});
        let ok = child("a", "succeeded");
        let failed = child("b", "failed");
        let running = child("c", "running");
        let children = [ok.clone(), failed.clone(), running.clone()];

        // Under `all`, any failed child fails the wait.
        assert_eq!(
            super::child_wait_outcome(&children, WaitMode::All, &[0, 1, 2]),
            Outcome::Failed
        );
        // Under `any`, only the first settled child decides the verdict, even
        // when a later child in the same snapshot failed.
        assert_eq!(
            super::child_wait_outcome(&children, WaitMode::Any, &[0]),
            Outcome::Ok
        );
        assert_eq!(
            super::child_wait_outcome(&children, WaitMode::Any, &[1]),
            Outcome::Failed
        );
        // Under `fail-fast`, the first failed child decides the verdict.
        assert_eq!(
            super::child_wait_outcome(&children, WaitMode::FailFast, &[1]),
            Outcome::Failed
        );
        let all_ok = [child("a", "succeeded"), child("b", "succeeded")];
        assert_eq!(
            super::child_wait_outcome(&all_ok, WaitMode::FailFast, &[0, 1]),
            Outcome::Ok
        );
        // A child still running when the wait ends early is not a failure.
        assert_eq!(
            super::child_wait_outcome(&[running], WaitMode::All, &[]),
            Outcome::Ok
        );
    }

    #[test]
    fn selected_mode_is_named_in_human_and_terminal_json_output() {
        for (mode, label) in [
            (WaitMode::All, "all"),
            (WaitMode::Any, "any"),
            (WaitMode::FailFast, "fail-fast"),
        ] {
            assert_eq!(mode_heading(mode), format!("wait mode: {label}\n"));
            let data = terminal_data(mode, Vec::new(), false, None);
            assert_eq!(data["mode"], label);
            assert_eq!(data["timed_out"], false);
            assert!(data["local_observation"].is_null());
        }
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

    #[test]
    fn pr_journal_obeys_the_selected_completion_policy() {
        let status = json!({
            "number": 7, "state": "OPEN", "head": "head", "draft": false,
            "merge": "clean", "review": "unknown", "error": null,
            "checks_passed": 0, "checks_total": 2,
            "checks_failed": 1, "checks_pending": 1
        });
        let event = json!({"status": status});
        assert_eq!(
            reading_pr_event(7, None, false, event.clone()).settle,
            Settle::Unsuccessful
        );
        assert_eq!(
            reading_pr_event(7, None, true, event).settle,
            Settle::Pending
        );
    }

    fn settled_pr_reading(head: &str, passed: bool, workflows: Option<bool>) -> Reading {
        let mut data = json!({
            "number": 7, "state": "OPEN", "head": head, "draft": false,
            "merge": "clean", "review": "unknown", "error": null,
            "checks_passed": u32::from(passed), "checks_total": 1,
            "checks_failed": u32::from(!passed), "checks_pending": 0
        });
        if let Some(active) = workflows {
            data["active_workflows"] = json!(active);
        }
        Reading {
            settle: if passed {
                Settle::Succeeded
            } else {
                Settle::Unsuccessful
            },
            line: "#7 settled".into(),
            data,
        }
    }

    #[test]
    fn a_new_head_stays_pending_until_actions_runs_exist_or_the_grace_ends() {
        let mut grace = ActionsGrace::new(Duration::from_secs(3600));
        let held = grace.apply(settled_pr_reading("a", true, None), "#7", false);
        assert_eq!(held.settle, Settle::Pending);
        assert!(held.line.starts_with("#7 pending"), "{}", held.line);
        assert!(held.line.contains("head=a"), "{}", held.line);

        // Runs for the head end the hold.
        for active in [true, false] {
            let seen = grace.apply(settled_pr_reading("a", true, Some(active)), "#7", false);
            assert_eq!(seen.settle, Settle::Succeeded);
        }

        // A default wait still settles on a failure at once; a complete one holds.
        let failed = settled_pr_reading("a", false, None);
        assert_eq!(
            grace.apply(failed.clone(), "#7", false).settle,
            Settle::Unsuccessful
        );
        assert_eq!(grace.apply(failed, "#7", true).settle, Settle::Pending);

        // An unreadable reading carries no line to hold.
        let unreadable = Reading::unavailable("#7", "boom");
        assert_eq!(grace.apply(unreadable.clone(), "#7", true), unreadable);
    }

    #[test]
    fn the_grace_period_is_bounded_and_restarts_for_a_new_head() {
        let mut ended = ActionsGrace::new(Duration::ZERO);
        assert_eq!(
            ended
                .apply(settled_pr_reading("a", true, None), "#7", false)
                .settle,
            Settle::Succeeded,
            "a zero grace period never holds"
        );

        let mut grace = ActionsGrace::new(Duration::from_secs(3600));
        assert_eq!(
            grace
                .apply(settled_pr_reading("a", true, None), "#7", false)
                .settle,
            Settle::Pending
        );
        assert!(!grace.is_due());
        let left = grace.due_in().expect("a held reading has a deadline");
        assert!(left > Duration::from_secs(3000), "{left:?}");
        // Age the head: the period has run out, so the reading settles.
        let (_, first_read) = grace.head.as_mut().unwrap();
        *first_read = Instant::now()
            .checked_sub(Duration::from_secs(7200))
            .expect("monotonic clock far enough from its origin");
        assert_eq!(
            grace
                .apply(settled_pr_reading("a", true, None), "#7", false)
                .settle,
            Settle::Succeeded
        );
        assert_eq!(grace.due_in(), None, "a settled reading is not held");
        // A later push is a new head with its own period.
        assert_eq!(
            grace
                .apply(settled_pr_reading("b", true, None), "#7", false)
                .settle,
            Settle::Pending
        );
    }

    #[test]
    fn a_pinned_wait_ignores_the_verdict_of_another_head() {
        let line: crate::pr::status::Line = serde_json::from_value(json!({
            "number": 7, "state": "OPEN", "head": "stale", "draft": false,
            "merge": "clean", "review": "approved", "error": null,
            "checks_passed": 1, "checks_total": 1,
            "checks_failed": 0, "checks_pending": 0
        }))
        .expect("fixture line");
        assert_eq!(line.verdict(), Some(true));
        let pinned = gate_line(line, Some("new"));
        assert_eq!(pinned.verdict(), None);
        assert_eq!(watched_head(&pinned), "new");
        assert_eq!(pinned.head, "stale");
    }

    #[test]
    fn pr_timeout_reports_the_last_known_check_summary_and_next_step() {
        let sources = vec![Source::Pr {
            number: 7,
            repository: Ok("owner/repo".into()),
            watch_head: None,
            complete: false,
        }];
        let pending = Reading {
            settle: Settle::Pending,
            line: "#7 pending".into(),
            data: json!({
                "number": 7, "state": "OPEN", "head": "abc", "draft": false,
                "merge": "clean", "review": "unknown", "error": null,
                "checks_passed": null, "checks_total": null,
                "checks_failed": null, "checks_pending": null
            }),
        };
        let settled = Reading {
            settle: Settle::Succeeded,
            line: "#7 ready".into(),
            data: json!({
                "number": 7, "state": "OPEN", "head": "abc", "draft": false,
                "merge": "clean", "review": "approved", "error": null,
                "checks_passed": 1, "checks_total": 1,
                "checks_failed": 0, "checks_pending": 0
            }),
        };
        let details = pr_timeout_details(&sources, &[Some(pending), Some(settled)]);
        assert_eq!(details.len(), 1, "only an unsettled PR gets a detail");
        assert!(details[0].contains("#7 OPEN abc"), "{details:?}");
        assert!(details[0].contains("checks=unavailable"), "{details:?}");
        assert!(
            details[0].contains("cause=checks_not_scheduled"),
            "{details:?}"
        );
        assert!(
            details[0].contains(crate::pr::status::NO_CHECKS_NEXT),
            "{details:?}"
        );
    }

    #[test]
    fn a_timed_out_complete_wait_keeps_its_last_known_line() {
        // A failure with checks still running settles a default wait but keeps a
        // complete wait pending, so only the complete wait has a last known line.
        let reading = Reading {
            settle: Settle::Pending,
            line: "#7 failing".into(),
            data: json!({
                "number": 7, "state": "OPEN", "head": "abc", "draft": false,
                "merge": "clean", "review": "unknown", "error": null,
                "checks_passed": 0, "checks_total": 2,
                "checks_failed": 1, "checks_pending": 1,
                "failing_checks": ["lint"], "pending_checks": ["build"]
            }),
        };
        let source = |complete| {
            vec![Source::Pr {
                number: 7,
                repository: Ok("owner/repo".into()),
                watch_head: None,
                complete,
            }]
        };
        let complete = pr_timeout_details(&source(true), &[Some(reading.clone())]);
        assert_eq!(complete.len(), 1, "{complete:?}");
        assert!(complete[0].contains("fail=1"), "{complete:?}");
        assert!(complete[0].contains("pending=1"), "{complete:?}");
        assert!(complete[0].contains("pending_names=build"), "{complete:?}");
        assert!(pr_timeout_details(&source(false), &[Some(reading)]).is_empty());
    }
}
