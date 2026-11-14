//! One event receiver for mixed status sources. Adapters retain the existing
//! sampled store/GitHub observations; only semantic changes enter the channel.
// This edge owns the real deadline for channel receives and source I/O.
#![allow(clippy::disallowed_methods)]
use provenant_cli::{Condition, Globals};
use provenant_core::CancelToken;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug)]
enum Target {
    Auto(String),
    Lane(provenant_cli::Target),
    Graph(String),
    Operation(String),
    Pr(u64),
}
impl Target {
    fn parse(value: &str) -> Result<Self> {
        let Some((kind, id)) = value.split_once(':') else {
            return Ok(Self::Auto(value.into()));
        };
        match kind {
            "operation" => provenant_core::OperationId::parse(id)
                .map(|id| Self::Operation(id.to_string()))
                .map_err(|error| error.to_string()),
            "lane" => id.parse().map(Self::Lane).map_err(str::to_owned),
            "graph"
                if !id.is_empty()
                    && id.len() <= 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) =>
            {
                Ok(Self::Graph(id.into()))
            }
            "pr" | "land" => id
                .parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .map(Self::Pr)
                .ok_or_else(|| "PR and land targets require a positive PR number".into()),
            _ => Err(
                "target must be lane:ID, graph:ID, operation:ID, pr:NUMBER or land:NUMBER".into(),
            ),
        }
    }
}

struct ReadFailure {
    message: String,
    retry: bool,
}
impl From<String> for ReadFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            retry: false,
        }
    }
}
impl From<&str> for ReadFailure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

impl From<provenant_fabric_runtime::coordination::facade::FacadeError> for ReadFailure {
    fn from(error: provenant_fabric_runtime::coordination::facade::FacadeError) -> Self {
        Self {
            retry: matches!(
                error,
                provenant_fabric_runtime::coordination::facade::FacadeError::Busy
            ),
            message: error.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Snapshot {
    terminal: bool,
    notable: bool,
    data: Value,
}
impl Snapshot {
    fn read(
        target: &Target,
        edge: &provenant_cli::run::Edge<'_>,
        session: &mut Option<provenant_cli::fabric::Context>,
        left: Duration,
        pr_read_timeout: Duration,
        cancel: &CancelToken,
    ) -> std::result::Result<Self, ReadFailure> {
        match target {
            Target::Operation(id) => {
                let reply = provenant_cli::run::status(
                    provenant_cli::Status {
                        session_stream: false,
                        stats: false,
                        by: "template".into(),
                        id: Some(format!("operation:{id}")),
                        wait: None,
                        target: Vec::new(),
                        until: Condition::All,
                        follow: false,
                        limit: 100,
                        cursor: None,
                        all: false,
                    },
                    true,
                    edge,
                );
                if let Some(error) = reply.result.error {
                    return Err(ReadFailure {
                        retry: error.code == provenant_run::ErrorKind::Contention.code(),
                        message: error.message,
                    });
                }
                Ok(Self {
                    terminal: true,
                    notable: true,
                    data: reply.result.data,
                })
            }
            Target::Lane(target) => {
                let rows = provenant_cli::run::status_wait_lanes(target, edge).map_err(|e| {
                    ReadFailure {
                        retry: matches!(
                            e.kind,
                            provenant_run::ErrorKind::Contention
                                | provenant_run::ErrorKind::PeerUnavailable
                        ),
                        message: e.message,
                    }
                })?;
                if rows.is_empty() {
                    return Err("named lane selection is empty".into());
                }
                let terminal = rows
                    .iter()
                    .all(|row| matches!(row.state, provenant_run::AttemptState::Terminal { .. }));
                let notable = terminal;
                let mut lanes: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        let lane = provenant_cli::run::project_lane(row, &[], edge.roots.state.as_path(), None, false);
                        let value = json!({"id": lane.attempt_id, "kind": "attempt", "graph_id": null, "node_id": lane.task_id, "attempt_id": lane.attempt_id, "state": lane.state, "reason": lane.reason});
                        value
                    })
                    .collect();
                lanes.sort_by_key(|v| v["attempt_id"].to_string());
                let detail = provenant_cli::run::status_wait_details(target, &rows, edge).map_err(
                    |error| ReadFailure {
                        retry: matches!(
                            error.kind,
                            provenant_run::ErrorKind::Contention
                                | provenant_run::ErrorKind::PeerUnavailable
                        ),
                        message: error.message,
                    },
                )?;
                let mut data = json!({"rows": lanes});
                if let Some(detail) = detail {
                    data["detail"] = json!(detail);
                }
                Ok(Self {
                    terminal,
                    notable,
                    data,
                })
            }
            Target::Graph(id) | Target::Auto(id) => {
                let rows = crate::fabric::read_status_rows(id, edge, session)
                    .map_err(ReadFailure::from)?;
                let Some(rows) = rows else {
                    if matches!(target, Target::Auto(_)) {
                        return Self::read(
                            &Target::Lane(id.parse().map_err(str::to_owned)?),
                            edge,
                            session,
                            left,
                            pr_read_timeout,
                            cancel,
                        );
                    }
                    return Err("graph checkpoint not found".into());
                };
                if rows.is_empty() {
                    return Err("named graph selection is empty".into());
                }
                let terminal = graph_rows_terminal(&rows);
                let notable = terminal || rows.iter().any(|row| row["state"] == "paused");
                Ok(Self {
                    terminal,
                    notable,
                    data: json!({"rows":rows}),
                })
            }
            Target::Pr(number) => {
                let row = crate::pr::status::wait_line(
                    edge.cwd,
                    edge.roots.state.as_path(),
                    *number,
                    left.min(pr_read_timeout),
                    crate::pr::observer::POLL,
                    Some(cancel),
                )
                .map_err(|e| {
                    let message = e.to_string();
                    let retry = provenant_core::retry::is_transient(
                        provenant_core::retry::RetryCause::CommandOutput {
                            stderr: &message,
                            stdout: "",
                        },
                    );
                    ReadFailure { message, retry }
                })?;
                let class = row.class();
                Ok(Self {
                    terminal: matches!(class, "merged" | "closed"),
                    notable: matches!(
                        class,
                        "merged" | "closed" | "ready" | "failing" | "review_required"
                    ),
                    data: json!({"class": class, "pr": row}),
                })
            }
        }
    }
}

/// Cancellation wakes adapter pauses and interrupts in-flight GitHub reads.
#[derive(Default)]
struct Stop {
    stopped: Mutex<bool>,
    wake: Condvar,
    cancel: CancelToken,
}
impl Stop {
    fn stop(&self) {
        self.cancel.cancel();
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

/// A settled graph or node row already proves its attempts drained, and attempt
/// rows use a wider vocabulary (`timed_out`, `quota_limited`, ...), so an attempt
/// decides only when no owner row was selected. A collected attempt keeps the
/// checkpoint's `terminal` row, which has no `finished_at`.
fn graph_rows_terminal(rows: &[Value]) -> bool {
    if let Some(graph) = rows.iter().find(|row| row["kind"] == "graph") {
        return matches!(
            graph["state"].as_str(),
            Some("complete" | "cancelled" | "paused" | "review_required" | "needs_owner_decision")
        );
    }
    let mut owners = rows
        .iter()
        .filter(|row| row["kind"] != "attempt")
        .peekable();
    if owners.peek().is_none() {
        return rows
            .iter()
            .all(|row| !row["finished_at"].is_null() || row["state"] == "terminal");
    }
    owners.all(|row| {
        matches!(
            row["state"].as_str(),
            Some(
                "complete"
                    | "cancelled"
                    | "succeeded"
                    | "failed"
                    | "blocked"
                    | "skipped"
                    | "terminal"
                    | "paused"
                    | "review_required"
                    | "needs_owner_decision"
            )
        )
    })
}

/// Longest a local target goes without a full read while its files look
/// untouched. The read samples sources a file stamp cannot see: peer hosts and
/// process liveness.
const IDLE_REFRESH: Duration = Duration::from_secs(10);

/// Size and modification time of one file a read depends on.
type FileStamp = Option<(u64, u64, i64, i64)>;

/// Cheap change hint for a local target: the metadata of the files its read
/// derives from. Interval ticks compare stamps and skip the full read, which
/// opens the repository, the Run store and the graph checkpoint, while nothing
/// has been written. `None` means the target has no file hint (a PR).
fn stamp(target: &Target, state: &Path) -> Option<Vec<FileStamp>> {
    let mut files = vec![state.join("run.sqlite3"), state.join("run.sqlite3-wal")];
    match target {
        Target::Lane(_) => {}
        Target::Graph(id) | Target::Auto(id) => {
            let graphs = state.join("fabric").join("graphs");
            let projects = std::fs::read_dir(&graphs).into_iter().flatten().flatten();
            for project in projects {
                files.push(project.path().join(format!("graph-{id}.json")));
                files.push(project.path());
            }
        }
        Target::Operation(_) | Target::Pr(_) => return None,
    }
    Some(
        files
            .iter()
            .map(|path| {
                let meta = std::fs::metadata(path).ok()?;
                Some((meta.ino(), meta.size(), meta.mtime(), meta.mtime_nsec()))
            })
            .collect(),
    )
}

/// Decides when a waiter pays for a full read. The stamp is taken before the
/// read, so a write that lands during it is seen on the next tick.
#[derive(Default)]
struct Reads {
    seen: Option<(Vec<FileStamp>, Instant)>,
}
impl Reads {
    fn due(&self, now: Instant, stamp: Option<&Vec<FileStamp>>) -> bool {
        match (&self.seen, stamp) {
            (Some((seen, at)), Some(stamp)) => {
                seen != stamp || now.duration_since(*at) >= IDLE_REFRESH
            }
            _ => true,
        }
    }
    fn record(&mut self, now: Instant, stamp: Option<Vec<FileStamp>>, read_ok: bool) {
        self.seen = stamp.filter(|_| read_ok).map(|stamp| (stamp, now));
    }
}

struct Update {
    target: String,
    snapshot: Result<Snapshot>,
}

/// The receiver owns completion; source latency never serialises other targets.
fn collect(
    receiver: &mpsc::Receiver<Update>,
    count: usize,
    deadline: Instant,
    until: Condition,
    follow: bool,
    once: bool,
    out: &mut impl FnMut(Value) -> Result<()>,
) -> Result<bool> {
    let mut latest = BTreeMap::<String, Snapshot>::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let update = match receiver.recv_timeout(left) {
            Ok(update) => update,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) if Instant::now() >= deadline => {
                return Ok(false);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("status sources disconnected before completion".into());
            }
        };
        let snapshot = update
            .snapshot
            .map_err(|e| format!("{}: {e}", update.target))?;
        let previous = latest.insert(update.target.clone(), snapshot.clone());
        let changed = previous.as_ref().is_some_and(|old| old != &snapshot);
        let initial = previous.is_none();
        if (follow && initial) || changed || (initial && snapshot.terminal) {
            out(
                json!({"event": if initial { "snapshot" } else { "transition" }, "target": update.target,
                "terminal": snapshot.terminal, "data": snapshot.data}),
            )?;
        }
        let all = latest.len() == count && (once || latest.values().all(|row| row.terminal));
        if all || (!follow && matches!(until, Condition::Any) && (changed || snapshot.terminal)) {
            return Ok(true);
        }
    }
}

pub(crate) fn run(command: provenant_cli::Status, globals: &Globals) -> u8 {
    use crate::roots::{Product, Workspace};
    let run = || -> Result<(bool, bool, bool)> {
        let resolved = crate::roots::resolve(globals, Workspace::Project, Product::Required)
            .map_err(|e| e.to_string())?;
        let settings =
            provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)
                .map_err(|e| e.to_string())?
                .domains
                .wait;
        // Targets without `--wait` report their current status once.
        let once = command.wait.is_none();
        let seconds = settings.timeout(command.wait);
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut names = command.target;
        if let Some(id) = command.id {
            names.push(id);
        }
        names.sort();
        names.dedup();
        if names.is_empty() || names.len() > settings.max_targets as usize {
            return Err(format!(
                "status --wait requires 1 to {} targets",
                settings.max_targets
            ));
        }
        let targets = names
            .into_iter()
            .map(|name| Target::parse(&name).map(|target| (name, target)))
            .collect::<Result<Vec<_>>>()?;
        let stop = Stop::default();
        let (sender, receiver) = mpsc::sync_channel(128);
        let edge = provenant_cli::run::Edge {
            roots: &resolved.roots,
            home: &resolved.home,
            assets: &resolved.assets,
            cwd: &resolved.project,
        };
        let pr_read_timeout = Duration::from_secs(settings.pr_read_timeout_seconds);
        let mut refused = false;
        let met = std::thread::scope(|scope| {
            for (name, target) in &targets {
                let sender = sender.clone();
                let edge = &edge;
                let stop = &stop;
                let interval = Duration::from_millis(if matches!(target, Target::Pr(_)) {
                    settings.pr_interval_ms
                } else {
                    settings.local_interval_ms
                });
                scope.spawn(move || {
                    let mut last = None;
                    let mut reads = Reads::default();
                    let mut session = None;
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() || stop.cancel.is_cancelled() {
                            break;
                        }
                        let stamped = stamp(target, edge.roots.state.as_path());
                        if !once && !reads.due(Instant::now(), stamped.as_ref()) {
                            if !stop.pause(interval.min(left)) {
                                break;
                            }
                            continue;
                        }
                        let snapshot = provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
                            Snapshot::read(
                                target,
                                edge,
                                &mut session,
                                left,
                                pr_read_timeout,
                                &stop.cancel,
                            )
                        });
                        reads.record(Instant::now(), stamped, snapshot.is_ok());
                        // Keep the last good row across read failures, as pr status does.
                        if snapshot
                            .as_ref()
                            .is_err_and(|error| error.retry || last.is_some())
                        {
                            if !stop.pause(
                                interval.min(deadline.saturating_duration_since(Instant::now())),
                            ) {
                                break;
                            }
                            continue;
                        }
                        let terminal = once || snapshot.as_ref().is_ok_and(|row| row.terminal);
                        let failed = snapshot.is_err();
                        if snapshot.as_ref().ok() != last.as_ref() || failed {
                            last = snapshot.as_ref().ok().cloned();
                            if sender
                                .send(Update {
                                    target: name.clone(),
                                    snapshot: snapshot.map_err(|error| error.message),
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        if terminal
                            || failed
                            || !stop.pause(
                                interval.min(deadline.saturating_duration_since(Instant::now())),
                            )
                        {
                            break;
                        }
                    }
                });
            }
            drop(sender);
            let result = collect(
                &receiver,
                targets.len(),
                deadline,
                command.until,
                command.follow || once,
                once,
                &mut |event| {
                    refused |= event["data"]["operation"]["state"] == "refused";
                    write_event(&event, globals.json)
                },
            );
            stop.stop();
            drop(receiver);
            result
        })?;
        Ok((met, refused, once))
    };
    match run() {
        Ok((met, refused, true)) => wait_exit(met, refused),
        Ok((met, refused, false)) => {
            let event = json!({"event": "end", "condition_met": met, "timed_out": !met});
            if let Err(error) = write_event(&event, globals.json) {
                return crate::fail(
                    "status",
                    provenant_records::Outcome::Error,
                    "IO",
                    &error,
                    globals,
                );
            }
            wait_exit(met, refused)
        }
        Err(error) => crate::fail(
            "status",
            provenant_records::Outcome::Error,
            "WAIT",
            &error,
            globals,
        ),
    }
}
fn wait_exit(met: bool, refused: bool) -> u8 {
    if !met {
        124
    } else if refused {
        1
    } else {
        0
    }
}
fn write_event(event: &Value, json: bool) -> Result<()> {
    use std::io::Write;
    let line = if json {
        event.to_string()
    } else if event["event"] == "end" {
        if event["timed_out"] == true {
            "wait timed out; targets remain running".into()
        } else {
            "wait condition met".into()
        }
    } else {
        format!(
            "{} {} {}",
            event["target"].as_str().unwrap_or(""),
            event["event"].as_str().unwrap_or(""),
            event["data"]
        )
    };
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
        .and_then(|_| stdout.flush())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
#[allow(dead_code)] // Also included by the worktree driver without libtest.
mod tests {
    use super::*;

    #[test]
    fn idle_target_skips_reads_until_its_files_change_or_the_refresh_lapses() {
        let now = Instant::now();
        let stamp = vec![Some((1, 10, 5, 0))];
        let mut reads = Reads::default();
        assert!(reads.due(now, Some(&stamp)), "first read is always due");
        reads.record(now, Some(stamp.clone()), true);
        let soon = now + IDLE_REFRESH / 2;
        assert!(!reads.due(soon, Some(&stamp)));
        assert!(reads.due(soon, Some(&vec![Some((1, 11, 6, 0))])));
        assert!(reads.due(now + IDLE_REFRESH, Some(&stamp)));
        assert!(
            reads.due(soon, None),
            "targets without a file hint always read"
        );
        reads.record(soon, Some(stamp.clone()), false);
        assert!(reads.due(soon, Some(&stamp)), "a failed read is retried");
    }
    #[test]
    fn busy_graph_read_is_retried_while_other_targets_run() {
        use provenant_fabric_runtime::coordination::facade::FacadeError;
        let busy = ReadFailure::from(FacadeError::Busy);
        assert!(busy.retry);
        assert!(!ReadFailure::from(FacadeError::NotFound).retry);
        assert_ne!(wait_exit(false, false), 0);
    }

    #[test]
    fn refusal_targets_are_terminal_failures_when_observed() {
        assert!(
            matches!(Target::parse("operation:fixture-op"), Ok(Target::Operation(id)) if id == "fixture-op")
        );
        assert!(Target::parse("operation:bad/id").is_err());
        assert_eq!(wait_exit(true, true), 1);
        assert_eq!(wait_exit(true, false), 0);
        assert_eq!(wait_exit(false, true), 124);
    }
    #[test]
    fn a_settled_graph_is_terminal_whatever_its_attempt_state() {
        let graph = |state: &str| json!({"kind": "graph", "state": state});
        let attempt = |state: &str, finished: Value| json!({"kind": "attempt", "state": state, "finished_at": finished});
        let done = json!("2026-11-07T00:00:00Z");
        assert!(graph_rows_terminal(&[
            graph("complete"),
            attempt("timed_out", done.clone())
        ]));
        assert!(graph_rows_terminal(&[
            graph("paused"),
            json!({"kind":"node", "state":"pending"}),
            attempt("running", Value::Null)
        ]));
        assert!(!graph_rows_terminal(&[
            graph("pending"),
            attempt("timed_out", done.clone())
        ]));
        assert!(graph_rows_terminal(&[attempt("quota_limited", done)]));
        assert!(!graph_rows_terminal(&[attempt("running", Value::Null)]));
        assert!(graph_rows_terminal(&[attempt("terminal", Value::Null)]));
    }

    fn update(target: &str, state: &str, terminal: bool) -> Update {
        Update {
            target: target.into(),
            snapshot: Ok(Snapshot {
                terminal,
                notable: terminal,
                data: json!({"state": state}),
            }),
        }
    }
    #[test]
    fn mixed_receiver_returns_on_first_transition_without_waiting_for_other_sources() {
        let (tx, rx) = mpsc::channel();
        for event in [
            update("lane:a", "running", false),
            update("graph:b", "pending", false),
            update("pr:1", "pending", false),
            update("lane:a", "succeeded", true),
        ] {
            assert!(tx.send(event).is_ok());
        }
        let mut events = Vec::new();
        assert_eq!(
            collect(
                &rx,
                3,
                Instant::now() + Duration::from_secs(1),
                Condition::Any,
                false,
                false,
                &mut |e| {
                    events.push(e);
                    Ok(())
                }
            ),
            Ok(true)
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["target"], "lane:a");
        assert_eq!(events[0]["event"], "transition");
    }
    #[test]
    fn follow_deduplicates_and_waits_for_every_terminal_target() {
        let (tx, rx) = mpsc::channel();
        for event in [
            update("lane:a", "running", false),
            update("pr:1", "pending", false),
            update("lane:a", "succeeded", true),
            update("lane:a", "succeeded", true),
            update("pr:1", "merged", true),
        ] {
            assert!(tx.send(event).is_ok());
        }
        let mut events = Vec::new();
        assert_eq!(
            collect(
                &rx,
                2,
                Instant::now() + Duration::from_secs(1),
                Condition::Any,
                true,
                false,
                &mut |e| {
                    events.push(e);
                    Ok(())
                }
            ),
            Ok(true)
        );
        assert_eq!(events.len(), 4);
        assert_eq!(events[3]["target"], "pr:1");
        assert_eq!(events[3]["terminal"], true);
    }
    #[test]
    fn all_finishes_when_every_target_is_settled() {
        let (tx, rx) = mpsc::channel();
        assert!(tx.send(update("lane:a", "succeeded", true)).is_ok());
        let paused = update("graph:b", "paused", true);
        assert!(tx.send(paused).is_ok());
        assert_eq!(
            collect(
                &rx,
                2,
                Instant::now() + Duration::from_millis(10),
                Condition::All,
                false,
                false,
                &mut |_| Ok(())
            ),
            Ok(true)
        );
    }
    #[test]
    fn initial_notable_snapshot_does_not_return_early_and_later_transition_does() {
        let (tx, rx) = mpsc::channel();
        let mut paused = update("graph:b", "paused", false);
        if let Ok(snapshot) = &mut paused.snapshot {
            snapshot.notable = true;
        }
        assert!(tx.send(paused).is_ok());
        let mut events = Vec::new();
        assert_eq!(
            collect(
                &rx,
                1,
                Instant::now() + Duration::from_millis(20),
                Condition::Any,
                false,
                false,
                &mut |e| {
                    events.push(e);
                    Ok(())
                }
            ),
            Ok(false)
        );
        assert!(events.is_empty());

        let (tx2, rx2) = mpsc::channel();
        let mut paused2 = update("graph:b", "paused", false);
        if let Ok(snapshot) = &mut paused2.snapshot {
            snapshot.notable = true;
        }
        assert!(tx2.send(paused2).is_ok());
        assert!(tx2.send(update("graph:b", "running", false)).is_ok());
        let mut events2 = Vec::new();
        assert_eq!(
            collect(
                &rx2,
                1,
                Instant::now() + Duration::from_secs(1),
                Condition::Any,
                false,
                false,
                &mut |e| {
                    events2.push(e);
                    Ok(())
                }
            ),
            Ok(true)
        );
        assert_eq!(events2.len(), 1);
        assert_eq!(events2[0]["target"], "graph:b");
        assert_eq!(events2[0]["event"], "transition");
    }
}
