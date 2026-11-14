//! Lane, pull request and worktree facts for Claude hooks. Each fact comes
//! from this binary's own read commands (`status`, `worktree list`, `pr status`)
//! so the hooks never read another command's store directly. A command that
//! is missing, slow or failing leaves its section marked unavailable. The
//! calls run in parallel under one deadline.
use provenant_cli::Globals;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_workflow::session::{
    CommandPrefix, LaneLine, SessionState, Unavailable, WorktreeLine,
};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const OUTPUT_CAP: usize = 512 * 1024;
const LANE_PAGE: &str = "100";
const LISTING_TIME: Duration = Duration::from_secs(3);

pub struct Want {
    pub worktrees: bool,
    /// Unix seconds; lanes settled after this count as new.
    pub settled_after_unix: i64,
    pub session: Option<String>,
}

/// This binary's own read commands, run in `cwd` with the hook's state root.
pub struct Cli<'a> {
    exe: PathBuf,
    state_dir: Option<&'a Path>,
    cwd: &'a Path,
    executor: &'a SystemExecutor,
}
impl<'a> Cli<'a> {
    pub fn new(
        globals: &'a Globals,
        cwd: &'a Path,
        executor: &'a SystemExecutor,
    ) -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        Ok(Self {
            exe,
            state_dir: globals.state_dir.as_deref(),
            cwd,
            executor,
        })
    }

    fn argv(&self, args: &[&str]) -> Vec<OsString> {
        let mut argv = vec![self.exe.clone().into_os_string(), "--json".into()];
        if let Some(state) = self.state_dir {
            argv.push("--state-dir".into());
            argv.push(state.as_os_str().to_owned());
        }
        argv.extend(args.iter().map(OsString::from));
        argv
    }

    /// The `data` of a successful command envelope, or a short cause.
    // The child's real-time budget is the hook's deadline.
    #[allow(clippy::disallowed_methods)]
    pub fn data(&self, args: &[&str], limit: Duration, deadline: Instant) -> Result<Value, String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let time = limit.min(remaining);
        if time < Duration::from_millis(50) {
            return Err("hook deadline reached".into());
        }
        let argv = self.argv(args);
        let mut options = ExecOptions::new(&argv, self.cwd, time);
        options.stdout = StreamPolicy::Capture { limit: OUTPUT_CAP };
        options.stderr = StreamPolicy::Capture { limit: 2048 };
        let out = self
            .executor
            .execute(options)
            .map_err(|_| "could not start".to_owned())?;
        if out.timed_out {
            return Err("timed out".into());
        }
        let envelope: Option<Value> = if out.stdout.truncated {
            None
        } else {
            serde_json::from_slice(&out.stdout.bytes).ok()
        };
        let Some(envelope) = envelope else {
            return Err(match out.termination {
                Termination::Exit(0) => "unreadable output".into(),
                Termination::Exit(code) => format!("exit {code}"),
                _ => "terminated".into(),
            });
        };
        if envelope["status"] == "ok" && out.termination == Termination::Exit(0) {
            return Ok(envelope["data"].clone());
        }
        let message = envelope["error"]["message"].as_str().unwrap_or("refused");
        Err(provenant_workflow::session::clean(message, 120))
    }

    /// Preserve complete inventory records even when the child hits its budget.
    #[allow(clippy::disallowed_methods)]
    fn worktree_listing(&self, deadline: Instant) -> Result<(Vec<WorktreeLine>, usize), String> {
        let time = LISTING_TIME.min(deadline.saturating_duration_since(Instant::now()));
        if time < Duration::from_millis(50) {
            return Err("hook deadline reached".into());
        }
        let argv = self.argv(&["worktree", "session-list"]);
        let mut options = ExecOptions::new(&argv, self.cwd, time);
        options.stdout = StreamPolicy::Capture { limit: OUTPUT_CAP };
        options.stderr = StreamPolicy::Capture { limit: 2048 };
        let out = self
            .executor
            .execute(options)
            .map_err(|_| "could not start".to_owned())?;
        parse_worktree_stream(&out.stdout.bytes).ok_or_else(|| {
            if out.timed_out {
                "timed out".into()
            } else {
                "inventory unavailable".into()
            }
        })
    }

    #[allow(clippy::disallowed_methods)]
    fn lane_listing(
        &self,
        deadline: Instant,
        settled_after: Option<i64>,
    ) -> Result<(Vec<LaneLine>, Option<String>, usize), String> {
        let time = LISTING_TIME.min(deadline.saturating_duration_since(Instant::now()));
        if time < Duration::from_millis(50) {
            return Err("hook deadline reached".into());
        }
        let mut args = vec!["status", "--session-stream", "--limit", LANE_PAGE];
        if settled_after.is_some() {
            args.push("--all");
        }
        let argv = self.argv(&args);
        let mut options = ExecOptions::new(&argv, self.cwd, time);
        options.stdout = StreamPolicy::Capture { limit: OUTPUT_CAP };
        options.stderr = StreamPolicy::Capture { limit: 2048 };
        let out = self
            .executor
            .execute(options)
            .map_err(|_| "could not start".to_owned())?;
        parse_lane_stream(&out.stdout.bytes, settled_after).ok_or_else(|| {
            if out.timed_out {
                "timed out".into()
            } else {
                "inventory unavailable".into()
            }
        })
    }
}

fn unavailable_sources(state: &mut SessionState, want: &Want, reason: String) {
    state.unavailable.push(Unavailable {
        source: "lanes".into(),
        reason: reason.clone(),
    });
    if want.worktrees {
        state.unavailable.push(Unavailable {
            source: "worktrees".into(),
            reason,
        });
    }
}

pub fn collect(globals: &Globals, cwd: &Path, want: &Want, deadline: Instant) -> SessionState {
    let mut state = SessionState {
        commands: CommandPrefix::new(globals.state_dir.as_deref(), cwd),
        ..Default::default()
    };
    let executor = match SystemExecutor::new(5) {
        Ok(executor) => executor,
        Err(error) => {
            unavailable_sources(
                &mut state,
                want,
                format!("command executor unavailable: {error}"),
            );
            return state;
        }
    };
    let cli = match Cli::new(globals, cwd, &executor) {
        Ok(cli) => cli,
        Err(error) => {
            unavailable_sources(
                &mut state,
                want,
                format!("current executable unavailable: {error}"),
            );
            return state;
        }
    };
    collect_with_cli(state, &cli, want, deadline)
}

fn collect_with_cli(
    mut state: SessionState,
    cli: &Cli<'_>,
    want: &Want,
    deadline: Instant,
) -> SessionState {
    std::thread::scope(|scope| {
        let active = scope.spawn(move || cli.lane_listing(deadline, None));
        let settled =
            scope.spawn(move || cli.lane_listing(deadline, Some(want.settled_after_unix)));
        let worktrees = want
            .worktrees
            .then(|| scope.spawn(move || cli.worktree_listing(deadline)));
        let custody = scope.spawn(move || {
            let mut args = vec!["status", "--mine", "--limit", LANE_PAGE];
            if let Some(session) = want.session.as_deref() {
                args.extend(["--session", session]);
            }
            cli.data(&args, LISTING_TIME, deadline)
        });
        match active.join() {
            Ok(Ok((lanes, next, skipped))) => {
                state.active = lanes;
                state.active_more = next.map(|cursor| next_page(&cursor, false, &state.commands));
                state.active_skipped = skipped;
            }
            Ok(Err(reason)) => state.unavailable.push(Unavailable {
                source: "active lanes".into(),
                reason,
            }),
            Err(_) => state.unavailable.push(Unavailable {
                source: "active lanes".into(),
                reason: "listing worker panicked".into(),
            }),
        }
        match settled.join() {
            Ok(Ok((lanes, next, skipped))) => {
                state.settled = lanes;
                state.settled_more = next.map(|cursor| next_page(&cursor, true, &state.commands));
                state.settled_skipped = skipped;
            }
            Ok(Err(reason)) => state.unavailable.push(Unavailable {
                source: "settled lanes".into(),
                reason,
            }),
            Err(_) => state.unavailable.push(Unavailable {
                source: "settled lanes".into(),
                reason: "listing worker panicked".into(),
            }),
        }
        if let Some(handle) = worktrees {
            match handle.join() {
                Ok(Ok((list, skipped))) => {
                    state.worktrees = list;
                    state.worktrees_skipped = skipped;
                }
                Ok(Err(reason)) => state.unavailable.push(Unavailable {
                    source: "worktrees".into(),
                    reason,
                }),
                Err(_) => state.unavailable.push(Unavailable {
                    source: "worktrees".into(),
                    reason: "listing worker panicked".into(),
                }),
            }
        }
        match custody.join() {
            Ok(Ok(data)) => {
                match serde_json::from_value::<provenant_records::custody::CustodyStatus>(data) {
                    Ok(view) => {
                        for item in view.items {
                            state
                                .custody
                                .push(format!("{} ({:?})", item.record.subject, item.presence));
                        }
                        state.custody_more = view.next.map(|cursor| {
                            state
                                .commands
                                .command(&format!("status --mine --cursor {cursor}"))
                        });
                    }
                    Err(error) => state.unavailable.push(Unavailable {
                        source: "custody".into(),
                        reason: format!("invalid custody response: {error}"),
                    }),
                }
            }
            Ok(Err(reason)) => state.unavailable.push(Unavailable {
                source: "custody".into(),
                reason,
            }),
            Err(_) => state.unavailable.push(Unavailable {
                source: "custody".into(),
                reason: "listing worker panicked".into(),
            }),
        }
    });
    state
}

fn text(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
/// A path as the commands print it: a string, or `{root, relative: {path}}`.
fn wire_path(value: &Value) -> Option<String> {
    if let Some(path) = value.as_str() {
        return Some(path.to_owned());
    }
    let relative = value
        .pointer("/relative/path")
        .or_else(|| value.get("path"))
        .and_then(Value::as_str)?;
    let root = value.get("root").and_then(Value::as_str).unwrap_or("");
    Some(format!(
        "{}/{}",
        root.trim_end_matches('/'),
        relative.trim_start_matches('/')
    ))
}
fn route(lane: &Value) -> String {
    let applied = lane.pointer("/applied_route/model").and_then(text);
    let requested = |key: &str| {
        lane.pointer(&format!("/requested_route/{key}"))
            .and_then(text)
    };
    applied
        .or_else(|| requested("model"))
        .or_else(|| requested("name"))
        .unwrap_or_else(|| "unresolved".into())
}
/// How to list the page after `cursor`, for the same listing the hook read.
/// Only the digits of a cursor are kept, since it reaches a command. A
/// non-default state directory the collector used is made absolute (against
/// the directory the collector ran in) and repeated, quoted for a POSIX shell.
/// A state directory that is not valid UTF-8 or holds control characters
/// cannot be quoted faithfully, so no runnable command is given, only a note.
fn next_page(cursor: &str, settled: bool, commands: &CommandPrefix) -> String {
    let cursor: String = cursor.chars().filter(char::is_ascii_digit).collect();
    let mut command = if settled {
        format!("status --session-stream --all --limit {LANE_PAGE}")
    } else {
        format!("status --session-stream --limit {LANE_PAGE}")
    };
    if !cursor.is_empty() {
        command.push_str(" --cursor ");
        command.push_str(&cursor);
    }
    commands.command(&command)
}
/// Lanes from a `status` listing. With `settled_after_unix`, only lanes that
/// finished later count. The cursor, when present, names a further page the
/// read did not take; settled lanes can finish out of queue order, so a later
/// page may still hold some, and the cursor is reported for both listings.
fn parse_lanes(data: &Value, settled_after_unix: Option<i64>) -> (Vec<LaneLine>, Option<String>) {
    let mut lanes = Vec::new();
    let rows = data["lanes"].as_array().map_or(&[][..], Vec::as_slice);
    for lane in rows {
        if let Some(after) = settled_after_unix {
            if !lane["state"].as_str().is_some_and(|state| {
                serde_json::from_value::<provenant_records::AttemptOutcome>(Value::String(
                    state.to_owned(),
                ))
                .is_ok()
            }) {
                continue;
            }
            let finished = lane["finished_at"]
                .as_str()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok())
                .map(|at| at.as_second());
            if finished.is_none_or(|at| at <= after) {
                continue;
            }
        }
        // The lane's target: its work key as `NAME/NODE#N`.
        let key = &lane["key"];
        let id = format!(
            "{}/{}#{}",
            key["graph"].as_str().unwrap_or("?"),
            key["node"].as_str().unwrap_or("?"),
            key["attempt"]
                .as_u64()
                .map_or_else(|| "?".to_owned(), |n| n.to_string())
        );
        lanes.push(LaneLine {
            id,
            state: lane["state"].as_str().unwrap_or("unknown").to_owned(),
            route: route(lane),
            worktree: wire_path(&lane["worktree"]),
            branch: lane.pointer("/delivery/branch").and_then(text),
            reason: lane["reason"].as_str().map(str::to_owned),
        });
    }
    (lanes, data["next_cursor"].as_str().map(str::to_owned))
}

/// The header describes one page; a timed-out partial page resumes after its
/// last complete record rather than skipping the unread tail of that page.
fn parse_lane_stream(
    bytes: &[u8],
    settled_after: Option<i64>,
) -> Option<(Vec<LaneLine>, Option<String>, usize)> {
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    let header = lines.next()?;
    if !header.ends_with(b"\n") {
        return None;
    }
    let header: Value = serde_json::from_slice(header).ok()?;
    let total = usize::try_from(header.get("total")?.as_u64()?).ok()?;
    if total > 100 {
        return None;
    }
    let next = header.get("next").and_then(Value::as_u64);
    let mut rows = Vec::new();
    let mut last = None;
    for line in lines {
        if !line.ends_with(b"\n") || rows.len() >= total {
            break;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            break;
        };
        let Some(cursor) = event.get("cursor").and_then(Value::as_u64) else {
            break;
        };
        let Some(lane) = event.get("lane") else {
            break;
        };
        rows.push(lane.clone());
        last = Some(cursor);
    }
    let skipped = total.saturating_sub(rows.len());
    let continuation = if skipped > 0 {
        Some(last.map_or_else(String::new, |n| n.to_string()))
    } else {
        next.map(|n| n.to_string())
    };
    let (lanes, _) = parse_lanes(&serde_json::json!({"lanes": rows}), settled_after);
    Some((lanes, continuation, skipped))
}
/// Registered worktrees other than the primary checkout, which `git` lists first.
fn parse_worktrees(data: &Value) -> Vec<WorktreeLine> {
    data.as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .skip(1)
        .map(|entry| WorktreeLine {
            path: wire_path(&entry["path"]).unwrap_or_else(|| "?".into()),
            branch: entry["branch"]
                .as_str()
                .map(|b| b.strip_prefix("refs/heads/").unwrap_or(b))
                .unwrap_or("detached")
                .to_owned(),
            dirty: entry["dirty"].as_str().unwrap_or("unknown").to_owned(),
            writer: entry["writer"].as_str().unwrap_or("unknown").to_owned(),
        })
        .collect()
}

/// Ignore a trailing partial record, but never treat it as a completed row.
fn parse_worktree_stream(bytes: &[u8]) -> Option<(Vec<WorktreeLine>, usize)> {
    let mut lines = bytes.split_inclusive(|byte| *byte == b'\n');
    let header = lines.next()?;
    if !header.ends_with(b"\n") {
        return None;
    }
    let total = serde_json::from_slice::<Value>(header)
        .ok()?
        .get("total")?
        .as_u64()?;
    let total = usize::try_from(total).ok()?;
    let mut entries = Vec::new();
    for line in lines {
        if !line.ends_with(b"\n") {
            break;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            break;
        };
        let Some(entry) = event.get("worktree") else {
            break;
        };
        if entries.len() >= total {
            break;
        }
        entries.push(entry.clone());
    }
    let skipped = total.saturating_sub(entries.len().max(1));
    Some((parse_worktrees(&Value::Array(entries)), skipped))
}

#[cfg(test)]
mod tests {

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn session_collection_keeps_lane_and_worktree_rows_after_deadline() {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("inventory");
        std::fs::write(
            &script,
            r##"#!/bin/sh
case "$*" in
  *worktree*) printf '%s\n' '{"total":3}' '{"worktree":{"path":"/primary"}}' '{"worktree":{"path":"/lane","branch":"refs/heads/lane"}}' ;;
  *--all*) printf '%s\n' '{"total":0,"next":null}' ;;
  *) printf '%s\n' '{"total":2,"next":null}' '{"cursor":2,"lane":{"key":{"graph":"g","node":"a","attempt":1},"attempt_id":"a","state":"running"}}' ;;
esac
/bin/sleep 5
"##,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executor = SystemExecutor::new(3).unwrap();
        let cli = Cli {
            exe: script,
            state_dir: None,
            cwd: temp.path(),
            executor: &executor,
        };
        let host_deadline = Instant::now() + Duration::from_secs(2);
        let state = collect_with_cli(
            SessionState::default(),
            &cli,
            &Want {
                worktrees: true,
                settled_after_unix: 0,
                session: None,
            },
            crate::hooks::session_collection_deadline(host_deadline),
        );
        let digest = state.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(digest.contains("g/a#1"), "{digest}");
        assert!(digest.contains("/lane"), "{digest}");
        assert!(
            digest.contains("1 lane record was not inspected"),
            "{digest}"
        );
        assert!(
            digest.contains("1 worktree skipped during inventory"),
            "{digest}"
        );
        // The child budget has expired, but the hook still has time to render
        // the partial digest into its native SessionStart response.
        assert!(Instant::now() < host_deadline);
        let root = std::fs::canonicalize(temp.path()).unwrap();
        for name in ["home", "checkout", "state"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        let mut inputs = provenant_core::RootInputs::new(root.join("home"), root.join("checkout"));
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        let commands = CommandPrefix::default();
        let context = provenant_workflow::session::hook_context_with(
            &roots.state,
            provenant_core::ProjectId::from_bytes([7; 16]),
            provenant_workflow::session::HookEvent::SessionStart,
            &br#"{"hook_event_name":"SessionStart","session_id":"test-session"}"#[..],
            &provenant_os::clock::SystemClock::new(),
            provenant_core::DurationMicros::from_micros(1_000_000),
            provenant_workflow::session::Advice {
                digest: &digest,
                commands: &commands,
                client: None,
            },
        );
        let output = String::from_utf8(context.stdout).unwrap();
        assert!(
            Instant::now() < host_deadline,
            "hook response missed its deadline"
        );
        for text in [
            "g/a#1",
            "/lane",
            "1 lane record was not inspected",
            "1 worktree skipped during inventory",
        ] {
            assert!(output.contains(text), "{output}");
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn lane_collector_keeps_rows_when_child_times_out() {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("lanes");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' '{\"total\":3,\"next\":7}' '{\"cursor\":20,\"lane\":{\"key\":{\"graph\":\"g\",\"node\":\"a\",\"attempt\":1},\"attempt_id\":\"a\",\"state\":\"running\"}}'\n/bin/sleep 5\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executor = SystemExecutor::new(1).unwrap();
        let cli = Cli {
            exe: script,
            state_dir: None,
            cwd: temp.path(),
            executor: &executor,
        };
        let (lanes, next, skipped) = cli
            .lane_listing(Instant::now() + Duration::from_secs(1), None)
            .unwrap();
        assert_eq!(lanes[0].id, "g/a#1");
        assert_eq!(next.as_deref(), Some("20"));
        assert_eq!(skipped, 2);
    }

    #[test]
    fn lane_stream_preserves_partial_page_and_resumes_after_last_record() {
        use super::*;
        let bytes = b"{\"total\":3,\"next\":7}\n{\"cursor\":20,\"lane\":{\"key\":{\"graph\":\"g\",\"node\":\"a\",\"attempt\":1},\"attempt_id\":\"a\",\"state\":\"running\"}}\n{\"cursor\":19";
        let (lanes, next, skipped) = parse_lane_stream(bytes, None).unwrap();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].id, "g/a#1");
        assert_eq!(next.as_deref(), Some("20"));
        assert_eq!(skipped, 2);
        let state = SessionState {
            active: lanes,
            active_more: next.map(|cursor| next_page(&cursor, false, &CommandPrefix::default())),
            active_skipped: skipped,
            ..Default::default()
        };
        let digest = state.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(digest.contains("g/a#1"), "{digest}");
        assert!(
            digest.contains("2 lane records were not inspected"),
            "{digest}"
        );
        assert!(digest.contains("--cursor 20"), "{digest}");
        let (_, next, skipped) = parse_lane_stream(b"{\"total\":1,\"next\":null}\n", None).unwrap();
        assert_eq!(next.as_deref(), Some(""));
        assert_eq!(skipped, 1);
        let settled = b"{\"total\":2,\"next\":null}\n{\"cursor\":2,\"lane\":{\"state\":\"running\"}}\n{\"cursor\":1,\"lane\":{\"key\":{\"graph\":\"g\",\"node\":\"done\",\"attempt\":1},\"attempt_id\":\"done\",\"state\":\"failed\",\"finished_at\":\"2026-10-27T01:00:00Z\"}}\n";
        let (lanes, next, skipped) = parse_lane_stream(settled, Some(0)).unwrap();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].id, "g/done#1");
        assert_eq!(next, None);
        assert_eq!(skipped, 0);
    }

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn worktree_collector_keeps_rows_when_child_times_out() {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("inventory");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' '{\"total\":4}' '{\"worktree\":{\"path\":\"/primary\"}}' '{\"worktree\":{\"path\":\"/one\",\"branch\":\"refs/heads/one\"}}'\n/bin/sleep 5\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executor = SystemExecutor::new(1).unwrap();
        let cli = Cli {
            exe: script,
            state_dir: None,
            cwd: temp.path(),
            executor: &executor,
        };
        let (trees, skipped) = cli
            .worktree_listing(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].path, "/one");
        assert_eq!(skipped, 2);
    }

    #[test]
    fn worktree_stream_keeps_completed_rows_and_counts_skipped() {
        use super::*;
        let stream = b"{\"total\":4}\n{\"worktree\":{\"path\":\"/primary\"}}\n{\"worktree\":{\"path\":\"/one\",\"branch\":\"refs/heads/one\",\"dirty\":\"clean\",\"writer\":\"free\"}}\n{\"worktree\":{\"path\":\"/incomplete\"";
        let (trees, skipped) = parse_worktree_stream(stream).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!(trees.len(), 1);
        assert_eq!(trees[0].path, "/one");
        let state = SessionState {
            worktrees: trees,
            worktrees_skipped: skipped,
            active: vec![LaneLine {
                id: "run/task/attempt".into(),
                state: "running".into(),
                route: "fixture".into(),
                worktree: None,
                branch: None,
                reason: None,
            }],
            ..Default::default()
        };
        let digest = state.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(digest.contains("/one"), "{digest}");
        assert!(digest.contains("run/task/attempt"), "{digest}");
        assert!(
            digest.contains("2 worktrees skipped during inventory"),
            "{digest}"
        );
        let single = SessionState {
            worktrees_skipped: 1,
            ..Default::default()
        };
        assert!(
            single
                .digest(provenant_workflow::session::DIGEST_LIMIT)
                .contains("1 worktree skipped during inventory")
        );
        assert!(parse_worktree_stream(b"{\"total\":4}").is_none());
        let (empty, skipped) = parse_worktree_stream(b"{\"total\":4}\n").unwrap();
        assert!(empty.is_empty());
        assert_eq!(skipped, 3);
    }

    #[test]
    fn shared_cli_setup_failure_marks_each_requested_source() {
        use super::*;
        let want = Want {
            worktrees: true,
            settled_after_unix: 0,
            session: None,
        };
        let mut state = SessionState::default();
        unavailable_sources(&mut state, &want, "executable unavailable".into());

        let names: Vec<_> = state
            .unavailable
            .iter()
            .map(|unavailable| unavailable.source.as_str())
            .collect();
        assert_eq!(names, ["lanes", "worktrees"]);
        let digest = state.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(digest.contains("lanes unavailable: executable unavailable"));
        assert!(digest.contains("worktrees unavailable: executable unavailable"));
    }

    #[test]
    fn lanes_degrade_when_fields_are_absent() {
        use super::*;
        use serde_json::json;
        let data = json!({"lanes":[
            {"key":{"graph":"g","node":"a","attempt":1},"attempt_id":"a","state":"running",
             "applied_route":{"model":"m"}, "worktree":{"kind":"path","root":"/","relative":{"path":"w/x"}}},
            {"key":{"graph":"g","node":"b","attempt":1},"attempt_id":"b","state":"queued",
             "requested_route":{"kind":"named","name":"utility"}}
        ],"next_cursor":"7"});
        let (lanes, next) = parse_lanes(&data, None);
        assert_eq!(next.as_deref(), Some("7"));
        assert_eq!(lanes[0].id, "g/a#1");
        assert_eq!(lanes[0].route, "m");
        assert_eq!(lanes[0].worktree.as_deref(), Some("/w/x"));
        assert_eq!(lanes[1].route, "utility");
        assert_eq!(lanes[1].worktree, None);
    }

    #[test]
    fn settled_lanes_need_a_finish_after_the_baseline() {
        use super::*;
        use serde_json::json;
        let data = json!({"lanes":[
            {"key":{"graph":"g","node":"new","attempt":1},"attempt_id":"new","state":"failed","reason":"boom","finished_at":"2026-10-27T01:00:00Z"},
            {"key":{"graph":"g","node":"old","attempt":1},"attempt_id":"old","state":"succeeded","finished_at":"2026-10-27T00:00:00Z"},
            {"key":{"graph":"g","node":"none","attempt":1},"attempt_id":"none","state":"lost"}
        ],"next_cursor":null});
        let after = "2026-10-27T00:30:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second();
        let (lanes, _) = parse_lanes(&data, Some(after));
        assert_eq!(lanes.len(), 2);
        assert!(
            lanes
                .iter()
                .any(|lane| lane.id == "g/new#1" && lane.reason.as_deref() == Some("boom"))
        );
        assert!(lanes.iter().any(|lane| lane.id == "g/unsettled#1"));
    }

    #[test]
    fn a_truncated_settled_listing_says_so_and_names_the_next_page() {
        use super::*;
        use provenant_workflow::session::SessionState;
        use serde_json::json;
        // More than a page of lanes: the settled listing is newest-queued
        // first, so a lane on a later page can still have finished recently.
        let data = json!({"lanes":[
            {"key":{"graph":"g","node":"a","attempt":1},"attempt_id":"a","state":"failed","finished_at":"2026-10-27T01:00:00Z"}
        ],"next_cursor":"1042"});
        let (lanes, next) = parse_lanes(&data, Some(0));
        let cwd = Path::new("/work");
        let commands = |state_dir| CommandPrefix::new(state_dir, cwd);
        let command = next_page(&next.unwrap_or_default(), true, &commands(None));
        assert!(
            command.starts_with("provenant status --session-stream --all --limit 100"),
            "{command}"
        );
        assert!(command.ends_with("--cursor 1042"), "{command}");
        assert_eq!(
            next_page("1; rm -rf /", false, &commands(None)),
            "provenant status --session-stream --limit 100 --cursor 1"
        );
        // A non-default state directory is repeated, safely quoted.
        let custom = next_page("7", false, &commands(Some(Path::new("/s/it's here"))));
        assert_eq!(
            custom,
            "provenant --state-dir '/s/it'\\''s here' status --session-stream --limit 100 --cursor 7"
        );
        // A relative one is made absolute against the collector's directory.
        assert_eq!(
            next_page("7", false, &commands(Some(Path::new("rel/state")))),
            "provenant --state-dir '/work/rel/state' status --session-stream --limit 100 --cursor 7"
        );
        // Control characters and non-UTF-8 bytes give a note, never a command.
        use std::os::unix::ffi::OsStrExt;
        for bad in [
            Path::new("/s/a\nb"),
            Path::new(std::ffi::OsStr::from_bytes(b"/s/\xff")),
        ] {
            let note = next_page("7", false, &commands(Some(bad)));
            assert!(
                note.starts_with(
                    "page with: provenant status --session-stream --limit 100 --cursor 7 and"
                ) && note.contains("--state-dir this hook used")
                    && !note.contains("/s/"),
                "{note}"
            );
        }
        // The unread PR hint carries the same state directory.
        assert_eq!(
            commands(None).command("pr status 9"),
            "provenant pr status 9"
        );
        assert_eq!(
            commands(Some(Path::new("rel/state"))).command("pr status 9"),
            "provenant --state-dir '/work/rel/state' pr status 9"
        );
        let unread = SessionState {
            prs_unread: vec![provenant_workflow::session::PrUnread {
                number: 9,
                command: commands(Some(Path::new("rel/state"))).command("pr status 9"),
            }],
            ..Default::default()
        };
        let shown = unread.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(
            shown.contains(
                "#9 status unavailable; run `provenant --state-dir '/work/rel/state' pr status 9`"
            ),
            "{shown}"
        );
        let state = SessionState {
            settled: lanes,
            settled_more: Some(command),
            ..Default::default()
        };
        let digest = state.digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(
            digest.contains("settled since the last chair save (1+)"),
            "{digest}"
        );
        assert!(
            digest.contains("this count is partial; run `provenant status --session-stream --all"),
            "{digest}"
        );
        // A complete listing shows a plain count and no note.
        let complete = SessionState::default().digest(provenant_workflow::session::DIGEST_LIMIT);
        assert!(
            !complete.contains("partial") && !complete.contains("(0+)"),
            "{complete}"
        );
    }

    #[test]
    fn worktrees_skip_the_primary_and_strip_the_ref_prefix() {
        use super::*;
        use serde_json::json;
        let data = json!([
            {"path":"/p","branch":"refs/heads/main","dirty":"clean","writer":"live"},
            {"path":{"root":"/","relative":{"path":"p/.worktrees/a"}},"branch":"refs/heads/feat/a","dirty":"dirty","writer":"free"}
        ]);
        let list = parse_worktrees(&data);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].branch, "feat/a");
        assert_eq!(list[0].path, "/p/.worktrees/a");
    }
}
