//! `provenant wait`: block until graph targets or pull requests settle. A wait
//! is a reader: neither expiry nor cancellation stops the work. Every target
//! resolves before the wait starts, and each source polls on its own thread so
//! a slow read never delays another target.
// This edge owns the real deadline for channel receives and source I/O.
#![allow(clippy::disallowed_methods)]
use provenant_cli::Globals;
use provenant_core::CancelToken;
use provenant_records::Outcome;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
enum Source {
    Graph(provenant_cli::Target),
    UnreadableGraph(provenant_cli::Target, String),
    Pr(u64),
}
impl Source {
    fn label(&self) -> String {
        match self {
            Self::Graph(target) | Self::UnreadableGraph(target, _) => target.to_string(),
            Self::Pr(number) => format!("#{number}"),
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

/// Read one source. The flag is true when this host's stamped files fully
/// represent the reading, so later ticks may skip reads while they are
/// unchanged; see [`Reads`].
fn read(
    source: &Source,
    edge: &provenant_cli::run::Edge<'_>,
    session: &mut Option<provenant_cli::fabric::Context>,
    deadline: Instant,
    pr_read_timeout: Duration,
    cancel: &CancelToken,
) -> (Reading, bool) {
    let label = source.label();
    let left = deadline.saturating_duration_since(Instant::now());
    if cancel.is_cancelled() || left.is_zero() {
        return (
            Reading::unavailable(&label, "wait deadline or cancellation reached"),
            false,
        );
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
                    let host = session.as_ref().map(|context| &context.identity.host);
                    return (classify_rows(&label, &rows), rows_local(&rows, host));
                }
                Err(error) => Reading::unavailable(&label, &error.to_string()),
            }
        }
        Source::Pr(number) => match crate::pr::status::wait_line(
            edge.cwd,
            edge.roots.state.as_path(),
            *number,
            left.min(pr_read_timeout),
            crate::pr::observer::POLL,
            Some(cancel),
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
    };
    (reading, false)
}

/// Whether every attempt row belongs to `host`. Without a host (a relay from
/// inside a lane) nothing local backs the rows. Rows of another host were
/// enriched from that peer's own store, which no file here stamps.
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

/// Longest a local target goes without a full read while its files look
/// untouched. The read samples sources a file stamp cannot see, such as
/// process liveness.
const IDLE_REFRESH: Duration = Duration::from_secs(10);

/// Inode, size and modification time of one file a read depends on.
type FileStamp = Option<(u64, u64, i64, i64)>;

/// Cheap change hint for a graph source: the metadata of the Run store, its
/// WAL and the graph checkpoint directory (checkpoints publish by atomic
/// rename, so the directory changes with every write). Interval ticks compare
/// stamps and skip the full read, which opens the Run store and the checkpoint,
/// while nothing has been written. Once a read has opened the project, only
/// that project's directory is stamped. `None` means the source has no file
/// hint (a pull request).
fn stamp(
    source: &Source,
    state: &Path,
    project: Option<provenant_core::ProjectId>,
) -> Option<Vec<FileStamp>> {
    if !matches!(source, Source::Graph(_)) {
        return None;
    }
    let mut files = vec![state.join("run.sqlite3"), state.join("run.sqlite3-wal")];
    let graphs = state.join("fabric").join("graphs");
    match project {
        Some(project) => files.push(graphs.join(project.to_string())),
        None => files.extend(
            std::fs::read_dir(&graphs)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path()),
        ),
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
/// read, so a write that lands during it is seen on the next tick. Only a read
/// whose reading is fully represented by the stamped files arms the gate, and
/// the last tick before the deadline always reads, so a wait never times out on
/// a hint alone.
#[derive(Default)]
struct Reads {
    seen: Option<(Vec<FileStamp>, Instant)>,
}
impl Reads {
    fn due(
        &self,
        now: Instant,
        stamp: Option<&Vec<FileStamp>>,
        left: Duration,
        interval: Duration,
    ) -> bool {
        match (&self.seen, stamp) {
            (Some((seen, at)), Some(stamp)) => {
                left <= interval || seen != stamp || now.duration_since(*at) >= IDLE_REFRESH
            }
            _ => true,
        }
    }
    /// `gated` is true for a successful read of a locally represented source.
    fn record(&mut self, now: Instant, stamp: Option<Vec<FileStamp>>, gated: bool) {
        self.seen = stamp.filter(|_| gated).map(|stamp| (stamp, now));
    }
}

/// Cancellation wakes source pauses and interrupts in-flight GitHub reads.
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
    target: usize,
    reading: Reading,
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
    let (settings, max_origin) =
        match provenant_cli::config_reads::read_for(&resolved.roots, "wait") {
            Ok(config) => (
                config.domains.wait,
                config
                    .origins
                    .get("domains.wait.max_timeout_seconds")
                    .cloned(),
            ),
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
    let deadline = Instant::now()
        + Duration::from_secs(settings.timeout(command.timeout, max_origin.as_ref()));
    let stop = Stop::default();
    let mut sources: Vec<Source> = command.targets.iter().cloned().map(Source::Graph).collect();
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
    sources.extend(prs.into_iter().map(Source::Pr));
    if sources.len() > settings.max_targets as usize {
        return crate::fail(
            "wait",
            Outcome::Invalid,
            "TOO_MANY_TARGETS",
            &format!("wait takes at most {} targets", settings.max_targets),
            globals,
        );
    }
    let pr_read_timeout = Duration::from_secs(settings.pr_read_timeout_seconds);
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
    let done = std::thread::scope(|scope| {
        for (index, source) in sources.iter().enumerate() {
            if latest[index].as_ref().is_some_and(Reading::done) {
                continue;
            }
            let sampled = latest[index].is_some();
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let interval = Duration::from_millis(match source {
                Source::Pr(_) => settings.pr_interval_ms,
                Source::Graph(_) | Source::UnreadableGraph(..) => settings.local_interval_ms,
            });
            scope.spawn(move || {
                if sampled
                    && !stop.pause(interval.min(deadline.saturating_duration_since(Instant::now())))
                {
                    return;
                }
                let mut reads = Reads::default();
                let mut session = None;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if stop.cancel.is_cancelled() {
                        break;
                    }
                    let project = session
                        .as_ref()
                        .map(|context: &provenant_cli::fabric::Context| context.identity.project);
                    let stamped = stamp(source, edge.roots.state.as_path(), project);
                    if !left.is_zero()
                        && !reads.due(Instant::now(), stamped.as_ref(), left, interval)
                    {
                        if !stop.pause(interval.min(left)) {
                            break;
                        }
                        continue;
                    }
                    let (reading, local) = read(
                        source,
                        edge,
                        &mut session,
                        deadline,
                        pr_read_timeout,
                        &stop.cancel,
                    );
                    reads.record(Instant::now(), stamped, local);
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
    crate::emit(
        &provenant_records::CommandResult::new(
            "wait",
            outcome,
            json!({"targets":rows,"timed_out":!done}),
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
        let (reading, _) = read(
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

    #[test]
    fn idle_source_skips_reads_until_its_files_change_or_the_refresh_lapses() {
        let now = Instant::now();
        let (base, far) = (Duration::from_millis(500), Duration::from_secs(60));
        let stamp = vec![Some((1, 10, 5, 0))];
        let mut reads = Reads::default();
        assert!(
            reads.due(now, Some(&stamp), far, base),
            "first read is always due"
        );
        reads.record(now, Some(stamp.clone()), true);
        let soon = now + IDLE_REFRESH / 2;
        assert!(!reads.due(soon, Some(&stamp), far, base));
        assert!(reads.due(soon, Some(&vec![Some((1, 11, 6, 0))]), far, base));
        assert!(reads.due(now + IDLE_REFRESH, Some(&stamp), far, base));
        assert!(
            reads.due(soon, None, far, base),
            "sources without a file hint always read"
        );
        assert!(
            reads.due(now, Some(&stamp), base, base),
            "the last tick before the deadline reads whatever the stamp says"
        );
        reads.record(soon, Some(stamp.clone()), false);
        assert!(
            reads.due(soon, Some(&stamp), far, base),
            "a failed read, or one the files do not fully represent, is never gated"
        );
    }

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
