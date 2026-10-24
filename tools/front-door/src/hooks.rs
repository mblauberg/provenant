//! Native hook transports. No ordinary envelope, runtime stdin persistence or provider launch.
use provenant_cli::{GitHook, Globals, Hook, hooks::Handler};
use provenant_confinement::{
    UtilityBudget, UtilityRuntime,
    push::{PushContext, PushUpdate},
};
use provenant_core::{CancelToken, DurationMicros, GitOid};
use provenant_supervisor::{
    clock::SystemClock,
    exec::SystemExecutor,
    fs::{LocalFs, SafeFs},
    sys::hook_io,
};
use provenant_workflow::session::{HookEvent, HostSessionId};
use serde::Deserialize;
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
const INPUT_CAP: usize = 1024 * 1024;
const LOCAL_BUDGET: Duration = Duration::from_secs(1);
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
}
#[derive(Deserialize)]
struct AdvisoryInput {
    hook_event_name: HookEvent,
    session_id: String,
    cwd: PathBuf,
    source: Option<String>,
}

pub fn run(hook: Hook, globals: &Globals) -> u8 {
    let deadline = Instant::now() + LOCAL_BUDGET;
    match hook.spec().handler {
        Handler::ChairPrompt => crate::chair::hook(false, globals),
        Handler::ChairStart => crate::chair::hook(true, globals),
        Handler::Guard => guard(globals, deadline),
        Handler::PreCompact => advisory(HookEvent::PreCompact, globals, deadline),
        Handler::Compact => advisory(HookEvent::SessionStart, globals, deadline),
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
    if event.tool_name.is_empty() {
        return deny("hook input rejected: bounded-input error (invalid tool)");
    }
    if event.tool_name != "Bash" {
        return 0;
    }
    let Ok(tool) = serde_json::from_value::<ToolInput>(event.tool_input) else {
        return deny("hook input rejected: bounded-input error (invalid Bash command)");
    };
    if provenant_confinement::shell_command::check(&tool.command).is_err() {
        return deny(provenant_confinement::shell_command::DENY_REASON);
    }
    let cwd = event
        .cwd
        .filter(|cwd| cwd.is_absolute())
        .or_else(|| std::env::current_dir().ok());
    if let Some(cwd) = cwd {
        let config = guard_config(globals, &cwd);
        if let Some(remedy) = provenant_confinement::guard::violation(&tool.command, &cwd, &config)
        {
            return deny(remedy);
        }
    }
    if Instant::now() >= deadline {
        return deny("hook input rejected: bounded-input error (policy timed out)");
    }
    0
}
/// The `[domains.guard]` rules. Unreadable config keeps every rule on, since
/// the rules only refuse patterns and the remedy is always available.
fn guard_config(globals: &Globals, cwd: &Path) -> provenant_confinement::guard::GuardConfig {
    let read = || -> Result<_, Error> {
        let assets = crate::roots::product_root()?;
        let roots = crate::roots::roots(globals, &assets, cwd)?;
        let domains = provenant_config::read_with_domains(
            &roots,
            &LocalFs,
            &provenant_cli::domains::SECTIONS,
        )?
        .domains;
        Ok(domains.guard)
    };
    read().unwrap_or_default()
}
fn advisory(event: HookEvent, globals: &Globals, deadline: Instant) -> u8 {
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
            host.hook_event_name == event
                && host.cwd.is_absolute()
                && HostSessionId::parse(&host.session_id).is_ok()
                && (event != HookEvent::SessionStart || host.source.as_deref() == Some("compact"))
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
    // Read-only advisory work can be abandoned at the local deadline. No
    // publication, stdin readers or child custody is transferred to this thread.
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("checkpoint-hook".into())
        .spawn(move || {
            let result = checkpoint(event, &selected, &host.cwd, &input).map_err(|_| ());
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
fn checkpoint(
    event: HookEvent,
    globals: &Globals,
    cwd: &Path,
    input: &[u8],
) -> Result<provenant_workflow::session::HookContext, Error> {
    let assets = crate::roots::product_root()?;
    let project = std::fs::canonicalize(cwd)?;
    if let Some(selected) = &globals.project
        && std::fs::canonicalize(selected)? != project
    {
        return Err("hook project differs from cwd".into());
    }
    if !project.is_dir() {
        return Err("hook cwd unavailable".into());
    }
    let roots = crate::roots::roots(globals, &assets, &project)?;
    with_git(|runtime, executor, budget, cancel| {
        let location = runtime
            .project_location(executor, &project, budget, cancel)?
            .location;
        let binding = provenant_config::lookup_project(&roots, &location, &LocalFs)?
            .ok_or("project not bound")?;
        Ok(provenant_workflow::session::hook_context(
            &roots.state,
            binding.project_id(),
            event,
            input,
            &SystemClock::new(),
            DurationMicros::from_micros(24 * 60 * 60 * 1_000_000),
        ))
    })
}
fn with_git<T>(
    operation: impl FnOnce(
        &UtilityRuntime,
        &SystemExecutor,
        UtilityBudget,
        &CancelToken,
    ) -> Result<T, Error>,
) -> Result<T, Error> {
    let temp = std::fs::canonicalize(std::env::temp_dir())?;
    let anchor = LocalFs.open_anchor(&temp)?;
    let name = LocalFs.temporary_name()?;
    anchor.mkdir(&name, 0o700)?;
    let result = (|| {
        // Lanes grant this exact system binary; interactive hooks keep their
        // usual installed-Git discovery.
        let lane = std::env::var_os(provenant_run::LANE_DEPTH_ENV).is_some();
        let git = [
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
            "/opt/local/bin/git",
            "/usr/bin/git",
            "/bin/git",
        ]
        .into_iter()
        .map(Path::new)
        .filter(|p| !lane || *p == Path::new("/usr/bin/git"))
        .find(|p| provenant_supervisor::sys::is_executable(p))
        .ok_or("Git unavailable")?;
        let git = std::fs::canonicalize(git)?;
        let runtime = UtilityRuntime::new(Arc::new(LocalFs), git, temp.join(&name))?;
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
