//! Native hook transports. No ordinary envelope, runtime stdin persistence or provider launch.
use provenant_cli::{GitHook, Globals, Hook, hooks::Handler};
use provenant_confinement::{
    UtilityBudget, UtilityRuntime,
    push::{PushContext, PushUpdate},
};
use provenant_core::{CancelToken, Clock, DurationMicros, GitOid, WaitBudget};
use provenant_os::{clock::SystemClock, exec::SystemExecutor, sys::hook_io};
use provenant_workflow::session::{HookEvent, HostSessionId};
use serde::Deserialize;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
const INPUT_CAP: usize = 1024 * 1024;
const LOCAL_BUDGET: Duration = Duration::from_secs(1);
const ANSWER_BUDGET: Duration = Duration::from_millis(50);
/// Cap for hooks that read lane and PR state through this binary's own commands.
const SESSION_BUDGET: Duration = Duration::from_secs(4);
type Error = Box<dyn std::error::Error>;

#[derive(Deserialize)]
struct ToolInput {
    command: String,
}
#[derive(Deserialize)]
struct GuardInput {
    hook_event_name: String,
    tool_name: String,
    tool_input: serde_json::Value,
    cwd: Option<PathBuf>,
    session_id: Option<String>,
}
#[derive(Deserialize)]
struct AdvisoryInput {
    hook_event_name: String,
    session_id: String,
    cwd: PathBuf,
    source: Option<String>,
}

// Hook I/O and local work are constrained by real monotonic deadlines.
#[allow(clippy::disallowed_methods)]
pub fn run(hook: Hook, globals: &Globals) -> u8 {
    let started = Instant::now();
    let deadline = started + LOCAL_BUDGET;
    let client = hook.spec().registration.harness;
    match hook.spec().handler {
        Handler::ChairPrompt => crate::chair::hook(false, globals),
        Handler::ChairStart => crate::chair::hook(true, globals),
        Handler::Guard => guard(globals, deadline),
        Handler::PreCompact => advisory(Moment::PreCompact, globals, started, client),
        Handler::Compact => advisory(Moment::Compact, globals, started, client),
        Handler::SessionStart => advisory(Moment::Restart, globals, started, client),
        Handler::Stop => advisory(Moment::Stop, globals, started, client),
        Handler::WorktreeCreate => crate::hook_worktree::create(globals),
        Handler::WorktreeRemove => crate::hook_worktree::remove(globals),
        Handler::PrePush => {
            let Hook::Git(GitHook::PrePush { remote, .. }) = hook else {
                return 1;
            };
            match pre_push(&remote, globals, deadline) {
                Ok(()) => 0,
                Err(error) => {
                    diagnostic(&format!("pre-push: refused: {error}"));
                    1
                }
            }
        }
    }
}
fn diagnostic(message: &str) {
    let _ = writeln!(
        io::stderr().lock(),
        "{}",
        message.chars().take(1024).collect::<String>()
    );
}
// Denial output must obey the hook's real-time protocol budget.
#[allow(clippy::disallowed_methods)]
fn deny(reason: &str) -> u8 {
    let response = serde_json::json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": reason}});
    let mut bytes = response.to_string().into_bytes();
    bytes.push(b'\n');
    if hook_io::write_stdout(&bytes, Instant::now() + LOCAL_BUDGET).is_ok() {
        0
    } else {
        diagnostic("hook input rejected: bounded-input error; denial response unavailable");
        2
    }
}
// The hook policy deadline comes from the host's real-time input budget.
#[allow(clippy::disallowed_methods)]
fn guard(globals: &Globals, deadline: Instant) -> u8 {
    let input = match hook_io::read_stdin(INPUT_CAP, deadline) {
        Ok(bytes) => bytes,
        Err(_) => {
            return deny(
                "hook input rejected: bounded-input error (unreadable, oversized or timed out)",
            );
        }
    };
    let parsed = serde_json::from_slice::<GuardInput>(&input)
        .ok()
        .filter(|v| v.hook_event_name == "PreToolUse");
    let Some(event) = parsed else {
        return deny("hook input rejected: bounded-input error (malformed event)");
    };
    match guard_reply(globals, event, deadline, provenant_cli::fabric::in_lane()) {
        Err(reason) => deny(reason),
        Ok(Some(bytes)) => {
            let _ = hook_io::write_stdout(&bytes, deadline);
            0
        }
        Ok(None) => 0,
    }
}

#[allow(clippy::disallowed_methods)]
fn guard_reply(
    globals: &Globals,
    event: GuardInput,
    deadline: Instant,
    in_lane: bool,
) -> Result<Option<Vec<u8>>, &'static str> {
    if event.tool_name.is_empty() {
        return Err("hook input rejected: bounded-input error (invalid tool)");
    }
    if event.tool_name == "Bash" {
        let Ok(tool) = serde_json::from_value::<ToolInput>(event.tool_input) else {
            return Err("hook input rejected: bounded-input error (invalid Bash command)");
        };
        let cwd = event
            .cwd
            .as_ref()
            .filter(|cwd| cwd.is_absolute())
            .cloned()
            .or_else(|| std::env::current_dir().ok());
        if let Some(cwd) = cwd {
            let policy = guard_policy(globals, &cwd);
            let ctx = provenant_confinement::guard::Context {
                cwd: &cwd,
                home: policy.home.as_deref(),
                state: policy.state.as_deref(),
            };
            if let Some(remedy) =
                provenant_confinement::guard::violation(&tool.command, &ctx, &policy.config)
            {
                return Err(remedy);
            }
        }
        if Instant::now() >= deadline {
            return Err("hook input rejected: bounded-input error (policy timed out)");
        }
    }
    if in_lane {
        return Ok(None);
    }
    let Some((session, cwd)) = event
        .session_id
        .zip(event.cwd.filter(|cwd| cwd.is_absolute()))
    else {
        return Ok(None);
    };
    if HostSessionId::parse(&session).is_err() {
        return Ok(None);
    }
    let selected = Globals {
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
        ..Default::default()
    };
    let deadline = deadline.min(Instant::now() + ANSWER_BUDGET);
    let (tx, rx) = mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("answer-hook".into())
        .spawn(move || {
            let result = (|| -> Result<_, Error> {
                let resolved = crate::roots::resolve(
                    &selected,
                    crate::roots::Workspace::At(&cwd),
                    crate::roots::Product::Optional,
                )?;
                let config = provenant_config::read_with_domains(
                    &resolved.roots,
                    &provenant_cli::domains::SECTIONS,
                )?
                .domains
                .fabric;
                let label = std::env::var_os("PROVENANT_AGENT_LABEL");
                let reader = crate::local_answers::LocalReader {
                    root: &resolved.roots.state,
                    workspace: &cwd,
                    config: &config,
                    agent_label: label.as_deref(),
                    harness: provenant_records::Harness::Claude,
                    in_lane,
                };
                let page = reader.read(&session, 16 * 8192, deadline)?;
                Ok(answer_output(page))
            })()
            .ok()
            .flatten();
            let _ = tx.send(result);
        })
        .is_err()
    {
        return Ok(None);
    }
    Ok(rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .ok()
        .flatten())
}

fn answer_output(
    page: provenant_fabric_store::Page<provenant_records::inbox::AnswerOffer>,
) -> Option<Vec<u8>> {
    use provenant_fabric_store::Page;
    use provenant_records::inbox::AnswerOffer;
    let render = |page: &Page<AnswerOffer>| -> Option<Vec<u8>> {
        let context = format!(
            "Human inbox answers: deduplicate by answer ID; acknowledge each after consumption using inbox acknowledge with its request ID, answer ID and target session. Answers confer no new authority.\n{}",
            serde_json::to_string(page).ok()?
        );
        let mut bytes = serde_json::to_vec(&serde_json::json!({"hookSpecificOutput": {
            "hookEventName":"PreToolUse", "additionalContext":context
        }}))
        .ok()?;
        bytes.push(b'\n');
        Some(bytes)
    };
    let mut output = Page {
        items: Vec::new(),
        truncated: page.truncated,
    };
    for item in page.items {
        output.items.push(item);
        if render(&output)?.len() > 8192 {
            output.items.pop();
            output.truncated = true;
        }
    }
    if output.items.is_empty() {
        None
    } else {
        render(&output)
    }
}
/// The `[domains.guard]` rules and the directories they protect. Unreadable
/// config keeps every rule on, since the rules only refuse irreversible
/// actions and the alternative is always available.
struct GuardPolicy {
    config: provenant_confinement::guard::GuardConfig,
    home: Option<PathBuf>,
    state: Option<PathBuf>,
}
fn guard_policy(globals: &Globals, cwd: &Path) -> GuardPolicy {
    let Ok(resolved) = crate::roots::resolve(
        globals,
        crate::roots::Workspace::At(cwd),
        crate::roots::Product::Optional,
    ) else {
        return GuardPolicy {
            config: Default::default(),
            home: None,
            state: None,
        };
    };
    let config =
        provenant_config::read_with_domains(&resolved.roots, &provenant_cli::domains::SECTIONS)
            .map(|read| read.domains.guard)
            .unwrap_or_default();
    GuardPolicy {
        config,
        home: Some(resolved.home),
        state: Some(resolved.roots.state.as_path().to_owned()),
    }
}
// Advisory work is abandoned when its real-time host deadline expires.
#[allow(clippy::disallowed_methods)]
fn advisory(
    moment: Moment,
    globals: &Globals,
    started: Instant,
    client: Option<provenant_records::Harness>,
) -> u8 {
    // Reading lane and PR state can take longer than a checkpoint read, so
    // these hooks hold a wider budget; the registered host timeouts are 5s.
    let deadline = started + SESSION_BUDGET;
    let input = match hook_io::read_stdin(INPUT_CAP, deadline) {
        Ok(bytes) => bytes,
        Err(_) => {
            diagnostic("checkpoint unavailable: bounded-input error");
            return 0;
        }
    };
    let parsed = serde_json::from_slice::<AdvisoryInput>(&input)
        .ok()
        .filter(|host| {
            host.hook_event_name == moment.event_name()
                && host.cwd.is_absolute()
                && HostSessionId::parse(&host.session_id).is_ok()
                && moment.accepts(host.source.as_deref())
        });
    let Some(host) = parsed else {
        diagnostic("checkpoint unavailable: invalid hook input");
        return 0;
    };
    let selected = Globals {
        state_dir: globals.state_dir.clone(),
        project: globals.project.clone(),
        ..Default::default()
    };
    // The work can be abandoned at the local deadline. Its only publication is
    // the checkpoint's atomic replacement under the checkpoint lock, which a
    // process exit mid-way leaves complete. No stdin reader or child custody is
    // transferred to this thread, and its children carry their own timeouts.
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("checkpoint-hook".into())
        .spawn(move || {
            let result =
                checkpoint(moment, &selected, &host.cwd, &input, deadline, client).map_err(|_| ());
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        diagnostic("checkpoint unavailable: local reader unavailable");
        return 0;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    match rx.recv_timeout(remaining) {
        Ok(Ok(context)) => {
            diagnostic(&context.diagnostic);
            if context.stdout.len() <= 8192 {
                let _ = hook_io::write_stdout(&context.stdout, deadline);
            }
        }
        _ => diagnostic("checkpoint unavailable: state unavailable or local deadline exceeded"),
    }
    0
}
/// What a hook does with the session checkpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Moment {
    /// SessionStart after compaction: checkpoint plus a fresh state digest.
    Compact,
    /// SessionStart on startup, resume or clear: the same report.
    Restart,
    /// Before compaction: save the current state into the checkpoint.
    PreCompact,
    /// Turn end: refresh an existing checkpoint's state, at most every few minutes.
    Stop,
}
impl Moment {
    fn event_name(self) -> &'static str {
        match self {
            Self::Compact | Self::Restart => "SessionStart",
            Self::PreCompact => "PreCompact",
            Self::Stop => "Stop",
        }
    }
    fn accepts(self, source: Option<&str>) -> bool {
        match self {
            Self::Compact => source == Some("compact"),
            Self::Restart => matches!(source, Some("startup" | "resume" | "clear")),
            Self::PreCompact | Self::Stop => true,
        }
    }
    fn event(self) -> HookEvent {
        match self {
            Self::Compact | Self::Restart => HookEvent::SessionStart,
            Self::PreCompact | Self::Stop => HookEvent::PreCompact,
        }
    }
}
/// Longest quiet gap before the Stop hook refreshes a checkpoint's state.
const STOP_INTERVAL: Duration = Duration::from_secs(10 * 60);
/// Settled lanes reported when no checkpoint dates the report.
const NO_CHECKPOINT_WINDOW: i64 = 24 * 60 * 60;

fn project_id(
    globals: &Globals,
    cwd: &Path,
) -> Result<(provenant_core::Roots, PathBuf, provenant_core::ProjectId), Error> {
    let project = std::fs::canonicalize(cwd)?;
    if let Some(selected) = &globals.project
        && std::fs::canonicalize(selected)? != project
    {
        return Err("hook project differs from cwd".into());
    }
    if !project.is_dir() {
        return Err("hook cwd unavailable".into());
    }
    let resolved = crate::roots::resolve(
        globals,
        crate::roots::Workspace::At(&project),
        crate::roots::Product::Required,
    )?;
    let roots = resolved.roots;
    let id = with_git(|runtime, executor, budget, cancel| {
        let location = runtime
            .project_location(executor, &project, budget, cancel)?
            .location;
        let binding =
            provenant_config::lookup_project(&roots, &location)?.ok_or("project not bound")?;
        Ok(binding.project_id())
    })?;
    Ok((roots, resolved.home, id))
}
fn checkpoint(
    moment: Moment,
    globals: &Globals,
    cwd: &Path,
    input: &[u8],
    deadline: Instant,
    client: Option<provenant_records::Harness>,
) -> Result<provenant_workflow::session::HookContext, Error> {
    use provenant_workflow::session;
    let (roots, home, project) = project_id(globals, cwd)?;
    let host: AdvisoryInput = serde_json::from_slice(input)?;
    let store = session::SessionStore::new(
        roots.state.clone(),
        project,
        HostSessionId::parse(&host.session_id)?,
    );
    let clock = SystemClock::new();
    let wall = clock.wall_now()?.as_micros() / 1_000_000;
    // Lane sessions only report; they never write state into a checkpoint.
    let in_lane = provenant_cli::fabric::in_lane();
    let current = store.read().ok().flatten();
    let mut digest = String::new();
    match moment {
        Moment::Compact | Moment::Restart if !in_lane => {
            let baseline = current.as_ref().map(session::baseline);
            let want = crate::hook_state::Want {
                worktrees: false,
                settled_after_unix: baseline
                    .as_ref()
                    .map_or(wall - NO_CHECKPOINT_WINDOW, |b| b.chair_saved_unix),
                pr_refs: baseline.map(|b| b.pr_refs).unwrap_or_default(),
            };
            let state = crate::hook_state::collect(globals, cwd, &want, deadline);
            digest = state.digest(session::DIGEST_LIMIT);
        }
        Moment::PreCompact | Moment::Stop if !in_lane => {
            // Quiet means neither the chair's checkpoint nor the last autosave
            // changed within the interval.
            let last = [
                current.as_ref().map(|checkpoint| checkpoint.modified_at),
                store.read_autosave().ok().flatten().map(|a| a.modified_at),
            ]
            .into_iter()
            .flatten()
            .map(|at| at.as_micros() / 1_000_000)
            .max();
            let quiet = current.is_some()
                && last.is_some_and(|modified| {
                    wall < modified || wall - modified >= STOP_INTERVAL.as_secs() as i64
                });
            // A turn ends often: refresh only a checkpoint that exists and has
            // been quiet. Compaction may create the first one.
            if moment == Moment::PreCompact || quiet {
                match autosave(
                    &store,
                    globals,
                    cwd,
                    current.as_ref(),
                    wall,
                    &clock,
                    deadline,
                ) {
                    Ok(note) => diagnostic(&note),
                    Err(error) => diagnostic(&format!(
                        "checkpoint state not auto-saved: {}",
                        session::clean(&error.to_string(), 160)
                    )),
                }
            }
        }
        _ => {}
    }
    append_session_warning(&mut digest, moment, in_lane, || {
        crate::disk_guard::session_warning(roots.state.as_path(), &home, cwd)
    });
    if moment == Moment::Stop {
        return Ok(session::HookContext {
            session_id: None,
            status: session::CheckpointStatus::Present,
            needs_saving: false,
            diagnostic: String::new(),
            stdout: Vec::new(),
        });
    }
    Ok(session::hook_context_with(
        &roots.state,
        project,
        moment.event(),
        input,
        &clock,
        DurationMicros::from_micros(24 * 60 * 60 * 1_000_000),
        session::Advice {
            digest: &digest,
            commands: &session::CommandPrefix::new(globals.state_dir.as_deref(), cwd),
            client,
        },
    ))
}

fn append_session_warning(
    digest: &mut String,
    moment: Moment,
    in_lane: bool,
    observe: impl FnOnce() -> Option<String>,
) {
    if !in_lane
        && matches!(moment, Moment::Compact | Moment::Restart)
        && let Some(warning) = observe()
    {
        if !digest.is_empty() {
            digest.push('\n');
        }
        digest.push_str(&warning);
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod disk_warning_tests {
    #[allow(unused_imports)] // Also included by the harness=false worktree driver.
    use super::{Moment, append_session_warning};

    #[test]
    fn session_warning_is_only_added_for_chair_starts() {
        for moment in [Moment::Restart, Moment::Compact] {
            let mut digest = String::from("state");
            append_session_warning(&mut digest, moment, false, || None);
            assert_eq!(digest, "state");
            append_session_warning(&mut digest, moment, false, || {
                Some("Disk pressure: low".into())
            });
            assert_eq!(digest, "state\nDisk pressure: low");
        }
        for (moment, lane) in [
            (Moment::Restart, true),
            (Moment::Compact, true),
            (Moment::PreCompact, false),
            (Moment::Stop, false),
        ] {
            let mut digest = String::new();
            let observed = std::cell::Cell::new(false);
            append_session_warning(&mut digest, moment, lane, || {
                observed.set(true);
                None
            });
            assert!(digest.is_empty());
            assert!(!observed.get());
        }
    }
}
/// Collect state and save it to the hook-owned file beside the checkpoint.
fn autosave(
    store: &provenant_workflow::session::SessionStore,
    globals: &Globals,
    cwd: &Path,
    current: Option<&provenant_workflow::session::Checkpoint>,
    wall: i64,
    clock: &SystemClock,
    deadline: Instant,
) -> Result<String, Error> {
    use provenant_workflow::session;
    let baseline = current.map(session::baseline);
    let want = crate::hook_state::Want {
        worktrees: true,
        settled_after_unix: baseline.as_ref().map_or(wall, |b| b.chair_saved_unix),
        pr_refs: baseline.map(|b| b.pr_refs).unwrap_or_default(),
    };
    let state = crate::hook_state::collect(globals, cwd, &want, deadline);
    let stamp = String::from(clock.wall_now()?);
    // The chair's checkpoint is never written here: the state goes to the
    // hook-owned file beside it.
    store.write_autosave(
        state.document(&stamp, session::BLOCK_LIMIT).as_bytes(),
        WaitBudget::cli(clock.mono_now(), 1)?,
        &CancelToken::new(),
        clock,
    )?;
    Ok(format!(
        "Checkpoint state auto-saved: {} lanes in flight, {} settled since the chair's save, {} PRs, {} worktrees.{}",
        state.active.len(),
        state.settled.len(),
        state.prs.len() + state.prs_unread.len(),
        state.worktrees.len(),
        if state.unavailable.is_empty() {
            String::new()
        } else {
            format!(" Unavailable: {}.", state.unavailable.join(", "))
        }
    ))
}
pub(crate) fn with_git<T>(
    operation: impl FnOnce(
        &UtilityRuntime,
        &SystemExecutor,
        UtilityBudget,
        &CancelToken,
    ) -> Result<T, Error>,
) -> Result<T, Error> {
    let temp = std::fs::canonicalize(std::env::temp_dir())?;
    let anchor = provenant_os::fs::Directory::open_anchor(&temp)?;
    let name = provenant_os::fs::temporary_name()?;
    anchor.mkdir(&name, 0o700)?;
    let result = (|| {
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let git = std::fs::canonicalize(git)?;
        let runtime = UtilityRuntime::new(git, temp.join(&name))?;
        let executor = SystemExecutor::new(1)?;
        let budget = UtilityBudget::bounded(Duration::from_secs(1))?;
        operation(&runtime, &executor, budget, &CancelToken::new())
    })();
    let _ = anchor.rmdir(&name);
    result
}
fn pre_push(remote: &str, globals: &Globals, deadline: Instant) -> Result<(), Error> {
    let bytes = hook_io::read_stdin(INPUT_CAP, deadline)?;
    let text = std::str::from_utf8(&bytes)?;
    let mut updates = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_ascii_whitespace().collect();
        if fields.len() != 4
            || fields
                .iter()
                .any(|f| f.len() > 1024 || !f.bytes().all(|b| b.is_ascii_graphic()))
            || !fields[2].starts_with("refs/")
        {
            return Err("invalid push input".into());
        }
        let local = GitOid::parse(fields[1])?;
        let remote_oid = GitOid::parse(fields[3])?;
        if std::mem::discriminant(&local) != std::mem::discriminant(&remote_oid) {
            return Err("mixed push object formats".into());
        }
        updates.push(PushUpdate {
            local,
            remote_ref: fields[2].into(),
            remote: remote_oid,
        });
    }
    let project = crate::roots::selected_project(globals.project.as_deref(), true)?;
    with_git(|runtime, executor, budget, cancel| {
        let repo = runtime.open_read_repo(executor, &project, budget, cancel)?;
        PushContext {
            repo: &repo,
            runtime,
            executor,
            budget,
            cancel,
        }
        .check(remote, &updates)
        .map_err(|error| {
            diagnostic(&format!("pre-push: refused: {error}"));
            error.into()
        })
    })
}

#[cfg(test)]
#[allow(dead_code, unused_imports, clippy::disallowed_methods)]
mod answer_delivery_tests {
    use super::*;
    use provenant_fabric_store::Page;
    use provenant_records::inbox::AnswerOffer;
    use serde_json::json;

    fn offer(text: &str, byte: u8) -> AnswerOffer {
        let answer = provenant_core::HumanAnswerId::from_bytes([byte; 16]);
        serde_json::from_value(json!({
            "answer": {"schema_version":1, "id":answer,
                "request_id":provenant_core::HumanRequestId::from_bytes([byte;16]),
                "request_revision":1, "action":"reply", "option_id":null, "text":text,
                "actor":provenant_core::ActorId::from_bytes([9;16]), "channel":"console",
                "answered_at":"2026-10-28T00:00:00.000000Z", "operation_id":"answer"},
            "delivery": {"schema_version":1, "answer_id":answer,
                "target_host":provenant_core::HostId::from_bytes([1;16]), "target_session":"chair",
                "target_actor":provenant_core::ActorId::from_bytes([2;16]), "transport":"fabric_mail",
                "message_id":provenant_core::MessageId::from_bytes([byte;16]), "state":"queued",
                "attempts":0, "last_attempt_at":null, "acknowledged_at":null, "error":null}
        })).unwrap()
    }

    #[test]
    fn answer_context_is_native_bounded_and_preserves_exact_text() {
        let exact = "Yes \"🦀\"\nkeep authority";
        let bytes = answer_output(Page {
            items: vec![offer(exact, 1)],
            truncated: false,
        })
        .unwrap();
        assert!(bytes.len() <= 8192);
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert!(
            wire["hookSpecificOutput"]
                .get("permissionDecision")
                .is_none()
        );
        let context = wire["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let page: Page<AnswerOffer> =
            serde_json::from_str(context.split_once("\n").unwrap().1).unwrap();
        assert_eq!(page.items[0].answer.text.as_deref(), Some(exact));
        assert!(context.contains("acknowledge"));
    }

    #[test]
    fn escaped_overflow_keeps_small_answers_deliverable() {
        let large = offer(&"\"".repeat(3500), 1);
        let small = offer("Proceed", 2);
        let bytes = answer_output(Page {
            items: vec![large, small.clone()],
            truncated: false,
        })
        .unwrap();
        assert!(bytes.len() <= 8192);
        let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let context = wire["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let page: Page<AnswerOffer> =
            serde_json::from_str(context.split_once("\n").unwrap().1).unwrap();
        assert_eq!(page.items, vec![small]);
        assert!(page.truncated);
        assert!(
            answer_output(Page {
                items: vec![],
                truncated: false
            })
            .is_none()
        );
    }
    struct Fixture {
        _temp: tempfile::TempDir,
        globals: Globals,
        cwd: PathBuf,
        root: provenant_core::StateRoot,
        identity: provenant_fabric_runtime::identity::Identity,
        request: provenant_core::HumanRequestId,
        answer: provenant_core::HumanAnswerId,
    }
    impl Fixture {
        fn new() -> Result<Self, Error> {
            use provenant_fabric_runtime::{
                identity::{Identity, inbox_address},
                inbox::*,
            };
            use provenant_records::inbox::*;
            let temp = tempfile::tempdir()?;
            let base = temp.path().canonicalize()?;
            let cwd = base.join("workspace");
            std::fs::create_dir(&cwd)?;
            let globals = Globals {
                state_dir: Some(base.join("state")),
                ..Default::default()
            };
            let resolved = crate::roots::resolve(
                &globals,
                crate::roots::Workspace::At(&cwd),
                crate::roots::Product::Optional,
            )?;
            let roots = resolved.roots;
            std::fs::create_dir(roots.state.as_path())?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                roots.state.as_path(),
                std::fs::Permissions::from_mode(0o700),
            )?;
            let config =
                provenant_config::read_with_domains(&roots, &provenant_cli::domains::SECTIONS)?
                    .domains
                    .fabric;
            let host = provenant_config::bind_host(&roots)?;
            let label = std::env::var_os("PROVENANT_AGENT_LABEL");
            let inbox = inbox_address(host, &config, label.as_deref(), Some("claude"))?;
            let project = provenant_core::ProjectId::from_bytes([2; 16]);
            let identity = Identity {
                project,
                host,
                inbox,
                client: "claude".into(),
                cwd: provenant_records::AbsolutePath::new(&cwd)?,
                common_dir: provenant_records::AbsolutePath::new(&cwd)?,
                federation: None,
            };
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(clock.mono_now(), 1)?;
            provenant_workflow::session::SessionStore::new(
                roots.state.clone(),
                project,
                HostSessionId::parse("chair")?,
            )
            .with_chair(host, identity.inbox.actor)
            .with_workspace(cwd.clone())
            .save(b"checkpoint", budget, &cancel, &clock)?;
            let service = InboxService::open(&roots.state, &identity, &clock, budget, &cancel)?;
            let InboxResult::Saved { receipt } = service.execute(
                InboxInput::Ask {
                    session: "chair".into(),
                    operation_id: None,
                    request: RequestDraft {
                        kind: RequestKind::Question,
                        title: "Next?".into(),
                        body: "Choose".into(),
                        blocking: false,
                        options: vec![],
                        recommended_option_id: None,
                        proceeding_with: None,
                    },
                },
                budget,
                &cancel,
            )?
            else {
                return Err("ask receipt".into());
            };
            let InboxResult::Saved { receipt } = service.execute(
                InboxInput::Answer {
                    id: receipt.request_id,
                    action: AnswerAction::Reply,
                    option_id: None,
                    text: Some("Proceed carefully".into()),
                    expect_revision: receipt.revision,
                    channel: InteractionChannel::Console,
                    operation_id: None,
                },
                budget,
                &cancel,
            )?
            else {
                return Err("answer receipt".into());
            };
            drop(service); // Production hooks commonly read after every writer closes.
            Ok(Self {
                _temp: temp,
                globals,
                cwd,
                root: roots.state,
                identity,
                request: receipt.request_id,
                answer: receipt.answer_id.unwrap(),
            })
        }
        fn event(&self, tool: &str, command: &str) -> GuardInput {
            GuardInput {
                hook_event_name: "PreToolUse".into(),
                tool_name: tool.into(),
                tool_input: json!({"command":command}),
                cwd: Some(self.cwd.clone()),
                session_id: Some("chair".into()),
            }
        }
        fn acknowledge(&self) -> Result<(), Error> {
            use provenant_fabric_runtime::inbox::*;
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let budget = WaitBudget::cli(clock.mono_now(), 1)?;
            InboxService::open(&self.root, &self.identity, &clock, budget, &cancel)?.execute(
                InboxInput::Acknowledge {
                    id: self.request,
                    answer_id: self.answer,
                    session: "chair".into(),
                    operation_id: None,
                },
                budget,
                &cancel,
            )?;
            Ok(())
        }
    }

    #[test]
    #[allow(clippy::print_stderr)]
    fn queued_answers_preserve_guard_denials_repeat_and_disappear_after_ack() -> Result<(), Error> {
        let f = Fixture::new()?;
        let deadline = || Instant::now() + LOCAL_BUDGET;
        assert!(guard_reply(&f.globals, f.event("Bash", "rm -rf /"), deadline(), false).is_err());
        let mut timings = Vec::new();
        for tool in ["Bash", "Read"].into_iter().cycle().take(20) {
            let started = Instant::now();
            let bytes =
                guard_reply(&f.globals, f.event(tool, "git status"), deadline(), false)?.unwrap();
            timings.push(started.elapsed());
            assert!(bytes.len() <= 8192);
            let wire: serde_json::Value = serde_json::from_slice(&bytes)?;
            assert!(
                wire["hookSpecificOutput"]
                    .get("permissionDecision")
                    .is_none()
            );
            assert!(
                wire["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap()
                    .contains(&f.answer.to_string())
            );
        }
        timings.sort();
        assert!(
            timings[18] < Duration::from_millis(100),
            "p95={:?}",
            timings[18]
        );
        eprintln!(
            "allowed hook reply p95 (20 local debug samples): {:?}",
            timings[18]
        );
        assert!(guard_reply(&f.globals, f.event("Read", ""), deadline(), true)?.is_none());
        let mut missing = f.event("Read", "");
        missing.session_id = None;
        assert!(guard_reply(&f.globals, missing, deadline(), false)?.is_none());
        let mut wrong = f.event("Read", "");
        wrong.cwd = Some(f.cwd.parent().unwrap().to_owned());
        assert!(guard_reply(&f.globals, wrong, deadline(), false)?.is_none());
        assert!(guard_reply(&f.globals, f.event("Read", ""), Instant::now(), false)?.is_none());
        assert!(
            guard_reply(
                &f.globals,
                f.event("Bash", "git status"),
                Instant::now(),
                false
            )
            .is_err()
        );
        f.acknowledge()?;
        assert!(guard_reply(&f.globals, f.event("Read", ""), deadline(), false)?.is_none());
        Ok(())
    }
}
