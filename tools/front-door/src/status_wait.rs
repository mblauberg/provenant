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
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug)]
enum Target {
    Auto(String),
    Lane(provenant_cli::Target),
    Graph(String),
    Pr(u64),
}
impl Target {
    fn parse(value: &str) -> Result<Self> {
        let Some((kind, id)) = value.split_once(':') else {
            return Ok(Self::Auto(value.into()));
        };
        match kind {
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
            _ => Err("target must be lane:ID, graph:ID, pr:NUMBER or land:NUMBER".into()),
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
        left: Duration,
        pr_read_timeout: Duration,
        cancel: &CancelToken,
    ) -> std::result::Result<Self, ReadFailure> {
        match target {
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
                let terminal = rows.iter().all(|row| {
                    row.cloud_observation.0.map_or_else(
                        || matches!(row.state, provenant_run::AttemptState::Terminal { .. }),
                        |cloud| cloud.is_terminal(),
                    )
                });
                let notable = terminal;
                let mut lanes: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        let lane = provenant_cli::run::project_lane(row, &[], edge.roots.state.as_path(), &Default::default(), None, false);
                        let value = json!({"id": lane.attempt_id, "kind": "attempt", "graph_id": null, "node_id": lane.task_id, "attempt_id": lane.attempt_id, "state": lane.state, "reason": lane.reason});
                        value
                    })
                    .collect();
                lanes.sort_by_key(|v| v["attempt_id"].to_string());
                Ok(Self {
                    terminal,
                    notable,
                    data: json!({"rows": lanes}),
                })
            }
            Target::Graph(id) | Target::Auto(id) => {
                let rows =
                    crate::fabric::read_status_rows(id, edge).map_err(|error| ReadFailure {
                        retry: false,
                        message: error.to_string(),
                    })?;
                let Some(rows) = rows else {
                    if matches!(target, Target::Auto(_)) {
                        return Self::read(
                            &Target::Lane(id.parse().map_err(str::to_owned)?),
                            edge,
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
                let terminal = rows.iter().all(|row| {
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
                        )
                    )
                });
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
                    *number,
                    left.min(pr_read_timeout),
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
        let all = latest.len() == count && latest.values().all(|row| row.terminal);
        if all || (!follow && matches!(until, Condition::Any) && (changed || snapshot.terminal)) {
            return Ok(true);
        }
    }
}

pub(crate) fn run(command: provenant_cli::Status, globals: &Globals) -> u8 {
    use crate::roots::{Product, Workspace};
    let run = || -> Result<bool> {
        let resolved = crate::roots::resolve(globals, Workspace::Project, Product::Required)
            .map_err(|e| e.to_string())?;
        let settings =
            provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)
                .map_err(|e| e.to_string())?
                .domains
                .wait;
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
        std::thread::scope(|scope| {
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
                    loop {
                        let left = deadline.saturating_duration_since(Instant::now());
                        if left.is_zero() || stop.cancel.is_cancelled() {
                            break;
                        }
                        let snapshot = provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
                            Snapshot::read(target, edge, left, pr_read_timeout, &stop.cancel)
                        });
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
                        let terminal = snapshot.as_ref().is_ok_and(|row| row.terminal);
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
                command.follow,
                &mut |event| write_event(&event, globals.json),
            );
            stop.stop();
            drop(receiver);
            result
        })
    };
    match run() {
        Ok(met) => {
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
            if met { 0 } else { 124 }
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
    fn all_does_not_finish_on_one_terminal_or_a_notable_pause() {
        let (tx, rx) = mpsc::channel();
        assert!(tx.send(update("lane:a", "succeeded", true)).is_ok());
        let mut paused = update("graph:b", "paused", false);
        if let Ok(snapshot) = &mut paused.snapshot {
            snapshot.notable = true;
        }
        assert!(tx.send(paused).is_ok());
        assert_eq!(
            collect(
                &rx,
                2,
                Instant::now() + Duration::from_millis(10),
                Condition::All,
                false,
                &mut |_| Ok(())
            ),
            Ok(false)
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
