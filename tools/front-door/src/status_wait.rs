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
    sync::{Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
enum Source {
    Graph(provenant_cli::Target),
    Pr(u64),
}
impl Source {
    fn label(&self) -> String {
        match self {
            Self::Graph(target) => target.to_string(),
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
    let data = json!({ "rows": rows });
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

fn read(
    source: &Source,
    edge: &provenant_cli::run::Edge<'_>,
    left: Duration,
    pr_read_timeout: Duration,
    cancel: &CancelToken,
) -> Reading {
    let label = source.label();
    match source {
        Source::Graph(target) => {
            match provenant_sqlite::with_busy_timeout(Duration::ZERO, || {
                crate::fabric::read_status_rows(target, edge)
            }) {
                Ok(rows) => classify_rows(&label, &rows),
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
        let Ok(update) = receiver.recv_timeout(left) else {
            return false;
        };
        let index = update.target;
        if latest[index].as_ref() == Some(&update.reading) {
            continue;
        }
        changed(index, &update.reading);
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

pub(crate) fn run(command: provenant_cli::Wait, globals: &Globals) -> u8 {
    use crate::roots::{Product, Workspace};
    let resolved = match crate::roots::resolve(globals, Workspace::Project, Product::Required) {
        Ok(resolved) => resolved,
        Err(error) => {
            return crate::fail("wait", Outcome::Error, "ROOT", &error.to_string(), globals);
        }
    };
    let settings = match provenant_config::read_with_domains(
        &resolved.roots,
        &provenant_cli::domains::SECTIONS,
    ) {
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
    let mut sources: Vec<Source> = command.targets.iter().cloned().map(Source::Graph).collect();
    if let Err(error) = crate::fabric::check_targets(&command.targets, &edge) {
        return crate::fabric::command_fail("wait", error.as_ref(), globals);
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
        match crate::fabric::unsettled_graphs(&edge) {
            Ok(graphs) => sources.extend(graphs.into_iter().map(Source::Graph)),
            Err(error) => return crate::fabric::command_fail("wait", error.as_ref(), globals),
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
    let deadline = Instant::now() + Duration::from_secs(settings.timeout(command.timeout));
    let pr_read_timeout = Duration::from_secs(settings.pr_read_timeout_seconds);
    let mut latest: Vec<Option<Reading>> = vec![None; sources.len()];
    let mut settled = Vec::new();
    let mut write_error = None;
    let stop = Stop::default();
    let (sender, receiver) = mpsc::sync_channel(128);
    let done = std::thread::scope(|scope| {
        for (index, source) in sources.iter().enumerate() {
            let sender = sender.clone();
            let edge = &edge;
            let stop = &stop;
            let interval = Duration::from_millis(match source {
                Source::Pr(_) => settings.pr_interval_ms,
                Source::Graph(_) => settings.local_interval_ms,
            });
            scope.spawn(move || {
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if stop.cancel.is_cancelled() {
                        break;
                    }
                    let reading = read(source, edge, left, pr_read_timeout, &stop.cancel);
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
            &mut |index, reading| {
                if command.follow && write_error.is_none() {
                    let line = if globals.json {
                        json!({"event":"transition","target":sources[index].label(),
                            "settle":reading.settle,"line":reading.line})
                        .to_string()
                    } else {
                        reading.line.clone()
                    };
                    write_error = write_line(&line).err();
                }
            },
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
