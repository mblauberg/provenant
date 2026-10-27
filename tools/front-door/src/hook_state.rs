//! Lane, pull request and worktree facts for Claude hooks. Each fact comes
//! from this binary's own read commands (`lanes`, `worktree list`, `pr status`)
//! so the hooks never read another command's store directly. A command that
//! is missing, slow or failing leaves its section marked unavailable. The
//! calls run in parallel under one deadline.
use provenant_cli::Globals;
use provenant_os::exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination};
use provenant_workflow::session::{LaneLine, PrLine, SessionState, WorktreeLine};
use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const OUTPUT_CAP: usize = 512 * 1024;
const LANE_PAGE: &str = "100";
const LISTING_TIME: Duration = Duration::from_secs(2);
const PR_TIME: Duration = Duration::from_secs(3);
/// Outcomes after which a lane no longer runs. `input_required` is included:
/// the lane stopped and waits for the chair.
const TERMINAL: [&str; 15] = [
    "succeeded",
    "review_required",
    "failed",
    "rejected",
    "timed_out",
    "rate_limited",
    "quota_limited",
    "auth_required",
    "model_unavailable",
    "permission_denied",
    "quarantined",
    "lost",
    "uncommitted",
    "cancelled",
    "input_required",
];

pub struct Want {
    pub worktrees: bool,
    /// Unix seconds; lanes settled after this count as new.
    pub settled_after_unix: i64,
    pub pr_refs: Vec<u64>,
}

/// This binary's own read commands, run in `cwd` with the hook's state root.
pub struct Cli<'a> {
    exe: PathBuf,
    state_dir: Option<&'a Path>,
    cwd: &'a Path,
    executor: &'a SystemExecutor,
}
impl<'a> Cli<'a> {
    pub fn new(globals: &'a Globals, cwd: &'a Path, executor: &'a SystemExecutor) -> Option<Self> {
        Some(Self {
            exe: std::env::current_exe().ok()?,
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
}

pub fn collect(globals: &Globals, cwd: &Path, want: &Want, deadline: Instant) -> SessionState {
    let mut state = SessionState::default();
    let executor = SystemExecutor::new(4 + want.pr_refs.len()).ok();
    let cli = executor
        .as_ref()
        .and_then(|executor| Cli::new(globals, cwd, executor));
    let Some(cli) = cli else {
        state.unavailable.push("lanes".into());
        return state;
    };
    let mut terminal_args = vec!["lanes", "--all", "--limit", LANE_PAGE];
    for outcome in TERMINAL {
        terminal_args.extend(["--state", outcome]);
    }
    let (cli, terminal_args) = (&cli, &terminal_args);
    std::thread::scope(|scope| {
        let active =
            scope.spawn(move || cli.data(&["lanes", "--limit", LANE_PAGE], LISTING_TIME, deadline));
        let settled = scope.spawn(move || cli.data(terminal_args, LISTING_TIME, deadline));
        let worktrees = want
            .worktrees
            .then(|| scope.spawn(move || cli.data(&["worktree", "list"], LISTING_TIME, deadline)));
        let prs: Vec<_> = want
            .pr_refs
            .iter()
            .map(|number| {
                let number = *number;
                scope.spawn(move || {
                    let argument = number.to_string();
                    (
                        number,
                        cli.data(&["pr", "status", &argument], PR_TIME, deadline),
                    )
                })
            })
            .collect();
        match (active.join(), settled.join()) {
            (Ok(Ok(active)), Ok(Ok(settled))) => {
                let (lanes, next) = parse_lanes(&active, None);
                state.active = lanes;
                state.active_more =
                    next.map(|cursor| next_page(&cursor, false, cli.state_dir, cli.cwd));
                let (lanes, next) = parse_lanes(&settled, Some(want.settled_after_unix));
                state.settled = lanes;
                state.settled_more =
                    next.map(|cursor| next_page(&cursor, true, cli.state_dir, cli.cwd));
            }
            _ => state.unavailable.push("lanes".into()),
        }
        if let Some(handle) = worktrees {
            match handle.join() {
                Ok(Ok(list)) => state.worktrees = parse_worktrees(&list),
                _ => state.unavailable.push("worktrees".into()),
            }
        }
        for handle in prs {
            match handle.join() {
                Ok((number, Ok(data))) => state.prs.push(PrLine {
                    number,
                    status: pr_status(&data),
                }),
                Ok((number, Err(_))) => state.prs_unread.push(number),
                Err(_) => {}
            }
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
/// Single-quote `text` for a POSIX shell: a quote becomes `'\''`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
/// Starts a note that is shown as plain text instead of as a command.
const PLAIN_NOTE: &str = "page with: ";
/// How to list the page after `cursor`, for the same listing the hook read.
/// Only the digits of a cursor are kept, since it reaches a command. A
/// non-default state directory the collector used is made absolute (against
/// the directory the collector ran in) and repeated, quoted for a POSIX shell.
/// A state directory that is not valid UTF-8 or holds control characters
/// cannot be quoted faithfully, so no runnable command is given, only a note.
fn next_page(cursor: &str, settled: bool, state_dir: Option<&Path>, cwd: &Path) -> String {
    let cursor: String = cursor.chars().filter(char::is_ascii_digit).collect();
    let mut command = String::from("provenant");
    let mut note = false;
    if let Some(state) = state_dir {
        let absolute = cwd.join(state);
        match absolute
            .to_str()
            .filter(|s| !s.chars().any(char::is_control))
        {
            Some(text) => {
                command.push_str(" --state-dir ");
                command.push_str(&shell_quote(text));
            }
            None => note = true,
        }
    }
    command.push_str(&if settled {
        format!(" lanes --all --limit {LANE_PAGE}")
    } else {
        format!(" lanes --limit {LANE_PAGE}")
    });
    if settled {
        for outcome in TERMINAL {
            command.push_str(" --state ");
            command.push_str(outcome);
        }
    }
    if !cursor.is_empty() {
        command.push_str(" --cursor ");
        command.push_str(&cursor);
    }
    if note {
        format!(
            "{PLAIN_NOTE}{command} and the --state-dir this hook used (its path cannot be shown safely)"
        )
    } else {
        command
    }
}
/// Lanes from a `lanes` listing. With `settled_after_unix`, only lanes that
/// finished later count. The cursor, when present, names a further page the
/// read did not take; settled lanes can finish out of queue order, so a later
/// page may still hold some, and the cursor is reported for both listings.
fn parse_lanes(data: &Value, settled_after_unix: Option<i64>) -> (Vec<LaneLine>, Option<String>) {
    let mut lanes = Vec::new();
    let rows = data["lanes"].as_array().map_or(&[][..], Vec::as_slice);
    for lane in rows {
        if let Some(after) = settled_after_unix {
            let finished = lane["finished_at"]
                .as_str()
                .and_then(|at| at.parse::<jiff::Timestamp>().ok())
                .map(|at| at.as_second());
            if finished.is_none_or(|at| at <= after) {
                continue;
            }
        }
        let id = format!(
            "{}/{}/{}",
            lane["run_id"].as_str().unwrap_or("?"),
            lane["task_id"].as_str().unwrap_or("?"),
            lane["attempt_id"].as_str().unwrap_or("?")
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
/// A one-line summary of `pr status` data. It reads the scalar fields a PR
/// summary is likely to carry and falls back to the data's own JSON, so a
/// change to that command's fields degrades the line rather than the hook.
fn pr_status(data: &Value) -> String {
    let mut parts = Vec::new();
    for key in [
        "state",
        "ci",
        "checks",
        "review",
        "mergeable",
        "draft",
        "head",
    ] {
        let shown = match &data[key] {
            Value::String(value) => Some(value.chars().take(40).collect::<String>()),
            Value::Bool(value) => Some(value.to_string()),
            _ => None,
        };
        if let Some(value) = shown {
            parts.push(format!("{key}={value}"));
        }
    }
    if parts.is_empty() {
        return data.to_string().chars().take(120).collect();
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {

    #[test]
    fn lanes_degrade_when_fields_are_absent() {
        use super::*;
        use serde_json::json;
        let data = json!({"lanes":[
            {"run_id":"r","task_id":"t","attempt_id":"a","state":"running",
             "applied_route":{"model":"m"}, "worktree":{"kind":"path","root":"/","relative":{"path":"w/x"}}},
            {"run_id":"r","task_id":"t","attempt_id":"b","state":"queued",
             "requested_route":{"kind":"named","name":"utility"}}
        ],"next_cursor":"7"});
        let (lanes, next) = parse_lanes(&data, None);
        assert_eq!(next.as_deref(), Some("7"));
        assert_eq!(lanes[0].id, "r/t/a");
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
            {"run_id":"r","task_id":"t","attempt_id":"new","state":"failed","reason":"boom","finished_at":"2026-10-27T01:00:00Z"},
            {"run_id":"r","task_id":"t","attempt_id":"old","state":"succeeded","finished_at":"2026-10-27T00:00:00Z"},
            {"run_id":"r","task_id":"t","attempt_id":"none","state":"lost"}
        ],"next_cursor":null});
        let after = "2026-10-27T00:30:00Z"
            .parse::<jiff::Timestamp>()
            .unwrap()
            .as_second();
        let (lanes, _) = parse_lanes(&data, Some(after));
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].reason.as_deref(), Some("boom"));
    }

    #[test]
    fn a_truncated_settled_listing_says_so_and_names_the_next_page() {
        use super::*;
        use provenant_workflow::session::SessionState;
        use serde_json::json;
        // More than a page of lanes: the settled listing is newest-queued
        // first, so a lane on a later page can still have finished recently.
        let data = json!({"lanes":[
            {"run_id":"r","task_id":"t","attempt_id":"a","state":"failed","finished_at":"2026-10-27T01:00:00Z"}
        ],"next_cursor":"1042"});
        let (lanes, next) = parse_lanes(&data, Some(0));
        let cwd = Path::new("/work");
        let command = next_page(&next.unwrap_or_default(), true, None, cwd);
        assert!(
            command.starts_with("provenant lanes --all --limit 100 --state succeeded"),
            "{command}"
        );
        assert!(command.ends_with("--cursor 1042"), "{command}");
        assert_eq!(
            next_page("1; rm -rf /", false, None, cwd),
            "provenant lanes --limit 100 --cursor 1"
        );
        // A non-default state directory is repeated, safely quoted.
        let custom = next_page("7", false, Some(Path::new("/s/it's here")), cwd);
        assert_eq!(
            custom,
            "provenant --state-dir '/s/it'\\''s here' lanes --limit 100 --cursor 7"
        );
        // A relative one is made absolute against the collector's directory.
        assert_eq!(
            next_page("7", false, Some(Path::new("rel/state")), cwd),
            "provenant --state-dir '/work/rel/state' lanes --limit 100 --cursor 7"
        );
        // Control characters and non-UTF-8 bytes give a note, never a command.
        use std::os::unix::ffi::OsStrExt;
        for bad in [
            Path::new("/s/a\nb"),
            Path::new(std::ffi::OsStr::from_bytes(b"/s/\xff")),
        ] {
            let note = next_page("7", false, Some(bad), cwd);
            assert!(
                note.starts_with("page with: provenant lanes --limit 100 --cursor 7 and")
                    && note.contains("--state-dir this hook used")
                    && !note.contains("/s/"),
                "{note}"
            );
        }
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
            digest.contains("this count is partial; run `provenant lanes --all"),
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

    #[test]
    fn pr_status_reads_known_fields_or_falls_back_to_bounded_json() {
        use super::*;
        use serde_json::json;
        assert_eq!(
            pr_status(&json!({"state":"open","ci":"passing","draft":false,"head":"abc"})),
            "state=open ci=passing draft=false head=abc"
        );
        assert!(pr_status(&json!({"other": "x".repeat(500)})).len() <= 120);
    }
}
