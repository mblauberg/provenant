//! Opt-in chair handoff composition. Hooks read cached quota, never probe it.
use provenant_cli::{Chair, Globals};
use provenant_core::Roots;
use provenant_os::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
    sys::hook_io,
};
use provenant_providers::identity::RegisteredAccount;
use provenant_quota::{
    AccountRecord, QuotaSnapshot,
    chair::{SwitchDecision, decide_switch},
};
use provenant_records::{CommandResult, Outcome};
use provenant_workflow::{
    chair::{Handoff, HandoffStore},
    session::{HostSessionId, SessionStore},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Error = Box<dyn std::error::Error>;
#[derive(Deserialize)]
struct Input {
    hook_event_name: String,
    session_id: HostSessionId,
    cwd: PathBuf,
    transcript_path: Option<PathBuf>,
}
struct Context {
    roots: Roots,
    cwd: PathBuf,
    assets: PathBuf,
    home: PathBuf,
}
impl Context {
    fn new(globals: &Globals, cwd: &Path) -> Result<Self, Error> {
        terminal_path(cwd)?;
        let cwd = std::fs::canonicalize(cwd)?;
        terminal_path(&cwd)?;
        if !cwd.is_dir() {
            return Err("working directory unavailable".into());
        }
        if let Some(selected) = &globals.project
            && std::fs::canonicalize(selected)? != cwd
        {
            return Err("hook project differs from cwd".into());
        }
        let assets = crate::roots::product_root()?;
        Ok(Self {
            roots: crate::roots::roots(globals, &assets, &cwd)?,
            cwd,
            assets,
            home: crate::roots::home()?,
        })
    }
    fn store(&self) -> HandoffStore {
        HandoffStore::new(self.roots.state.clone(), self.cwd.clone())
    }
    fn edge(&self) -> provenant_cli::run::Edge<'_> {
        provenant_cli::run::Edge {
            roots: &self.roots,
            cwd: &self.cwd,
            home: &self.home,
            assets: &self.assets,
        }
    }
}
// Chair records need current UTC wall time at this production edge.
#[allow(clippy::disallowed_methods)]
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |t| t.as_secs() as i64)
}
fn current<'a>(
    accounts: &'a [RegisteredAccount],
    home: &Path,
) -> Result<&'a RegisteredAccount, Error> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let dir = std::fs::canonicalize(dir)?;
    let mut matches = accounts.iter().filter(|a| a.directory() == dir);
    let account = matches
        .next()
        .ok_or("current CLAUDE_CONFIG_DIR is not an explicitly registered Claude account")?;
    if matches.next().is_some() {
        return Err("current account registration is ambiguous".into());
    }
    Ok(account)
}
fn cached(roots: &Roots, key: &str) -> Option<QuotaSnapshot> {
    if !provenant_quota::component(key) {
        return None;
    }
    let dir = Directory::open_anchor(roots.state.as_path())
        .ok()?
        .open_verified_directory(OsStr::new("quota"))
        .ok()?
        .open_verified_directory(OsStr::new(key))
        .ok()?;
    let bytes = dir
        .read_bounded(OsStr::new("claude.json"), provenant_quota::MAX_RECORD_BYTES)
        .ok()?;
    let record = AccountRecord::decode(&bytes).ok()?;
    if record.account.adapter != "claude" || record.account.account_key != key {
        return None;
    }
    if record.last_error.is_some() {
        return None;
    }
    record.snapshot
}
fn quote(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    // Fish reads a backslash inside single quotes as an escape, so an apostrophe
    // and a backslash each take a double-quoted form, which sh, bash, zsh and
    // fish all read literally. Empty parts are skipped so no two single quotes
    // are adjacent (zsh RC_QUOTES).
    let mut quoted = String::new();
    let mut plain = String::new();
    for c in text.chars() {
        let literal = match c {
            '\'' => "\"'\"",
            '\\' => "\"\\\\\"",
            _ => {
                plain.push(c);
                continue;
            }
        };
        if !plain.is_empty() {
            quoted.push_str(&format!("'{plain}'"));
            plain.clear();
        }
        quoted.push_str(literal);
    }
    if !plain.is_empty() {
        quoted.push_str(&format!("'{plain}'"));
    }
    quoted
}
fn terminal_path(path: &Path) -> Result<&str, Error> {
    path.to_str()
        .filter(|text| !text.chars().any(char::is_control))
        .ok_or_else(|| "launch paths must be UTF-8 without control characters".into())
}
fn fixed_target(context: &Context, target: &RegisteredAccount) -> Result<(), Error> {
    let rotating = context.home.join(".claude");
    let rotating = std::fs::canonicalize(&rotating).unwrap_or(rotating);
    if target.directory() == rotating {
        return Err(
            "~/.claude rotates between accounts; use a fixed ~/.claude-<label> target".into(),
        );
    }
    Ok(())
}
// Hook deadlines must be checked against the real monotonic clock.
#[allow(clippy::disallowed_methods)]
fn check_time(deadline: Option<Instant>) -> Result<(), Error> {
    if deadline
        .is_some_and(|d| d.saturating_duration_since(Instant::now()) < Duration::from_secs(3))
    {
        return Err("hook deadline near; continuing without a switch".into());
    }
    Ok(())
}
fn command(
    context: &Context,
    target: &RegisteredAccount,
    checkpoint: &Path,
) -> Result<String, Error> {
    command_with_executable(context, target, checkpoint, &std::env::current_exe()?)
}
fn command_with_executable(
    context: &Context,
    target: &RegisteredAccount,
    checkpoint: &Path,
    executable: &Path,
) -> Result<String, Error> {
    // Supply these hooks to the new session even when the target has only its
    // own credentials/settings. No config directory contents are copied.
    fixed_target(context, target)?;
    let checkpoint = terminal_path(checkpoint)?;
    let hook = |leaf: &str| -> Result<String, Error> {
        Ok(format!(
            "{} hook claude {leaf}",
            quote(terminal_path(executable)?)
        ))
    };
    let settings = json!({"hooks":{
        "SessionStart":[{"hooks":[{"type":"command","command":hook("chair-start")?,"timeout":5}]}],
        "UserPromptSubmit":[{"hooks":[{"type":"command","command":hook("chair-prompt")?,"timeout":10}]}]
    }});
    let prompt = format!(
        "Continue the chair handoff from checkpoint {}. Read it and its state/handoff file first. Inspect the listed Fabric lane IDs in the run store before acting; collect existing results and never re-run completed lanes. Preserve the originating authority limits.",
        checkpoint
    );
    let script = format!(
        "cd {} && CLAUDE_CONFIG_DIR={} PROVENANT_STATE_DIR={} claude --settings {} {}",
        quote(terminal_path(&context.cwd)?),
        quote(terminal_path(target.directory())?),
        quote(terminal_path(context.roots.state.as_path())?),
        quote(&settings.to_string()),
        quote(&prompt)
    );
    // Herdr types this into the pane's login shell, which may be fish, so the
    // POSIX script is handed to /bin/sh as one quoted argument.
    Ok(format!("/bin/sh -c {}", quote(&script)))
}
fn execute_herdr(args: &[OsString]) -> Result<Value, Error> {
    let argv = std::iter::once(OsString::from("herdr"))
        .chain(args.iter().cloned())
        .collect::<Vec<_>>();
    let cwd = std::env::current_dir()?;
    let mut options = ExecOptions::new(&argv, &cwd, Duration::from_secs(1));
    options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
    options.stderr = StreamPolicy::Discard;
    let out = SystemExecutor::new(1)?.execute(options)?;
    if out.termination != Termination::Exit(0) || out.timed_out || out.stdout.truncated {
        return Err("Herdr pane launch unavailable".into());
    }
    Ok(serde_json::from_slice(&out.stdout.bytes)?)
}
fn launch(context: &Context, command: &str, deadline: Option<Instant>) -> Result<String, Error> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1")
        || std::env::var_os("HERDR_PANE_ID").is_none()
    {
        return Err("not running in a Herdr-managed pane".into());
    }
    check_time(deadline)?;
    let split = execute_herdr(&[
        "pane".into(),
        "split".into(),
        "--current".into(),
        "--direction".into(),
        "right".into(),
        "--cwd".into(),
        context.cwd.as_os_str().to_owned(),
        "--no-focus".into(),
    ])?;
    let pane = split
        .pointer("/result/pane/pane_id")
        .and_then(Value::as_str)
        .ok_or("Herdr did not return the new pane ID")?;
    check_time(deadline)?;
    execute_herdr(&["pane".into(), "run".into(), pane.into(), command.into()])?;
    Ok(pane.into())
}
fn switch(
    context: &Context,
    input: &Input,
    to: Option<&str>,
    dry_run: bool,
    automatic: bool,
    deadline: Option<Instant>,
) -> Result<Option<Value>, Error> {
    let config =
        provenant_config::read_with_domains(&context.roots, &provenant_cli::domains::SECTIONS)?;
    let knob = |name: &str| config.knobs.get(name).map(|k| &k.value);
    if automatic && knob("chair.auto_switch").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    check_time(deadline)?;
    if !dry_run && let Some(previous) = context.store().prepared(&input.session_id)? {
        return Ok(Some(prepared_response(context, &previous)));
    }
    let threshold = knob("chair.switch_threshold")
        .and_then(Value::as_u64)
        .unwrap_or(92);
    let allowed = knob("chair.switch_accounts")
        .and_then(Value::as_array)
        .ok_or("invalid switch allow-list")?;
    let accounts = crate::quota_live::registered_claude(&context.roots)?;
    let current = current(&accounts, &context.home)?;
    let snapshot = cached(&context.roots, current.key());
    let mut rotating_target = false;
    let targets = allowed
        .iter()
        .filter_map(Value::as_str)
        .filter(|label| *label != current.key() && to.is_none_or(|to| to == *label))
        .filter_map(|label| accounts.iter().find(|a| a.key() == label))
        .filter(|account| {
            let fixed = fixed_target(context, account).is_ok();
            rotating_target |= !fixed;
            fixed
        })
        .collect::<Vec<_>>();
    let snapshots = targets
        .iter()
        .map(|a| cached(&context.roots, a.key()))
        .collect::<Vec<_>>();
    let choices = targets
        .iter()
        .zip(&snapshots)
        .map(|(a, s)| (a.key(), s.as_ref()))
        .collect::<Vec<_>>();
    check_time(deadline)?;
    let decision = decide_switch(snapshot.as_ref(), &choices, threshold, now(), automatic);
    let (target, reason) = match decision {
        SwitchDecision::Stay => {
            let _ = writeln!(
                io::stderr().lock(),
                "chair switch: current quota below threshold or unavailable/stale; continuing"
            );
            return Ok(None);
        }
        SwitchDecision::Switch(label) => (
            targets.iter().find(|a| a.key() == label).copied(),
            "quota-aware chair handoff",
        ),
        SwitchDecision::Refuse(reason) => (
            None,
            if rotating_target {
                "~/.claude rotates between accounts; use a fixed ~/.claude-<label> target"
            } else {
                reason
            },
        ),
    };
    let store = context.store();
    if !dry_run && target.is_none() && store.recorded_refusal(&input.session_id)? {
        return Ok(None);
    }
    let path = store.path(&input.session_id);
    terminal_path(&path)?;
    let mut handoff = Handoff {
        session_id: input.session_id.clone(),
        cwd: context.cwd.clone(),
        transcript_path: input.transcript_path.clone(),
        state_handoff_file: None,
        created_unix: now(),
        open_lanes: vec![],
        target_account: target.map(|a| a.key().into()),
        reason: reason.into(),
        consumed: false,
    };
    if dry_run {
        return Ok(Some(
            json!({"outcome":if target.is_some() {"prepared"} else {"refused"},"dry_run":true,"target_account":handoff.target_account,"reason":reason,"checkpoint":path,"command":target.map(|a| command(context,a,&path)).transpose()?}),
        ));
    }
    check_time(deadline)?;
    let (project, lanes) =
        provenant_cli::run::chair_snapshot(&context.edge()).map_err(|e| e.message)?;
    check_time(deadline)?;
    handoff.open_lanes = lanes;
    if SessionStore::new(
        context.roots.state.clone(),
        project,
        input.session_id.clone(),
    )
    .read()?
    .is_some()
    {
        handoff.state_handoff_file = Some(context.roots.state.as_path().join(format!(
            "workflow/projects/{project}/sessions/{}/checkpoint.md",
            input.session_id.as_str()
        )));
    }
    check_time(deadline)?;
    let launch_command = target.map(|a| command(context, a, &path)).transpose()?;
    let (path, fresh) = store.save(&handoff)?;
    if !fresh && let Some(previous) = store.prepared(&input.session_id)? {
        return Ok(Some(prepared_response(context, &previous)));
    }
    let Some(target) = target else {
        if !fresh {
            return Ok(None);
        }
        return Ok(Some(
            json!({"outcome":"refused","reason":reason,"checkpoint":path,"dry_run":false}),
        ));
    };
    check_time(deadline)?;
    let command = launch_command.ok_or("launch command unavailable")?;
    let (pane, launch_error) = if fresh {
        match launch(context, &command, deadline) {
            Ok(pane) => (Some(pane), None),
            Err(error) => {
                check_time(deadline)?;
                (None, Some(error.to_string()))
            }
        }
    } else {
        (
            None,
            Some("handoff already prepared for this session; do not launch again".into()),
        )
    };
    Ok(Some(
        json!({"outcome":"prepared","dry_run":false,"target_account":target.key(),"checkpoint":path,"command":command,"pane":pane,"launch_error":launch_error}),
    ))
}
fn prepared_response(context: &Context, handoff: &Handoff) -> Value {
    let path = context.store().path(&handoff.session_id);
    let recovery = (|| -> Result<String, Error> {
        let label = handoff
            .target_account
            .as_deref()
            .ok_or("handoff target unavailable")?;
        let config =
            provenant_config::read_with_domains(&context.roots, &provenant_cli::domains::SECTIONS)?;
        if !config
            .knobs
            .get("chair.switch_accounts")
            .and_then(|k| k.value.as_array())
            .is_some_and(|labels| labels.iter().any(|v| v.as_str() == Some(label)))
        {
            return Err("handoff target is no longer allowed".into());
        }
        let accounts = crate::quota_live::registered_claude(&context.roots)?;
        let target = accounts
            .iter()
            .find(|a| a.key() == label)
            .ok_or("handoff target is no longer registered")?;
        command(context, target, &path)
    })();
    json!({"outcome":"already_prepared","dry_run":false,"target_account":handoff.target_account,"checkpoint":path,"command":recovery.as_ref().ok(),"launch_error":recovery.as_ref().err().map(|e|e.to_string())})
}
pub fn run(chair: Chair, globals: &Globals) -> u8 {
    let Chair::Switch {
        to,
        dry_run,
        session_id,
    } = chair;
    let result = (|| -> Result<Value, Error> {
        let cwd = crate::roots::selected_project(globals.project.as_deref(), false)?;
        let context = Context::new(globals, &cwd)?;
        let id = match session_id.or_else(|| {
            std::env::var("CLAUDE_CODE_SESSION_ID")
                .or_else(|_| std::env::var("CLAUDE_SESSION_ID"))
                .ok()
        }) {
            Some(id) => id,
            None if dry_run => "dry-run".into(),
            None => {
                return Err(
                    "a real switch requires --session-id, CLAUDE_CODE_SESSION_ID or CLAUDE_SESSION_ID"
                        .into(),
                );
            }
        };
        let input = Input {
            hook_event_name: "manual".into(),
            session_id: HostSessionId::parse(&id)?,
            cwd: context.cwd.clone(),
            transcript_path: None,
        };
        switch(&context, &input, to.as_deref(), dry_run, false, None)?
            .ok_or_else(|| "no switch decision".into())
    })();
    match result {
        Ok(value) => {
            let refused = value["outcome"] == "refused";
            let human = message(&value);
            let result = CommandResult::new(
                "chair switch",
                if refused {
                    Outcome::Refused
                } else {
                    Outcome::Ok
                },
                value,
            );
            crate::emit(&result, globals, &human, dry_run)
        }
        Err(error) => crate::fail(
            "chair switch",
            Outcome::Refused,
            "CHAIR_SWITCH",
            &error.to_string(),
            globals,
        ),
    }
}
fn message(value: &Value) -> String {
    if value["dry_run"] == true && value["target_account"].is_string() {
        return format!(
            "Dry run: target {}. Planned checkpoint: {}.\n{}",
            value["target_account"].as_str().unwrap_or("unknown"),
            value["checkpoint"].as_str().unwrap_or("unavailable"),
            value["command"].as_str().unwrap_or("")
        );
    }
    if value["outcome"] == "already_prepared" {
        let recovery = value["command"].as_str().map(|command| format!("If no successor session is running, use this exact recovery command:\n{command}"))
            .unwrap_or_else(|| format!("Recovery command unavailable: {}", value["launch_error"].as_str().unwrap_or("check the handoff target registration")));
        return format!(
            "Chair handoff already prepared. Inspect the successor before launching again. Checkpoint: {}. {recovery}",
            value["checkpoint"].as_str().unwrap_or("unavailable")
        );
    }
    if value["outcome"] == "refused" {
        return format!(
            "Chair switch refused: {}. Checkpoint log: {}",
            value["reason"].as_str().unwrap_or("unavailable"),
            value["checkpoint"].as_str().unwrap_or("unavailable")
        );
    }
    format!(
        "Chair handoff: target {}. Checkpoint: {}. Stop this session after the handoff. {}\n{}",
        value["target_account"].as_str().unwrap_or("unknown"),
        value["checkpoint"].as_str().unwrap_or("unavailable"),
        value["pane"]
            .as_str()
            .map(|p| format!(
                "Opened pane {p}; launch admission is not proof of session readiness."
            ))
            .unwrap_or_else(|| format!(
                "{}; run this exact command:",
                value["launch_error"].as_str().unwrap_or("dry run")
            )),
        value["command"].as_str().unwrap_or("")
    )
}
// The hook protocol imposes real wall-time input and execution caps.
#[allow(clippy::disallowed_methods)]
pub fn hook(start: bool, globals: &Globals) -> u8 {
    // Stay inside the registered 5s SessionStart and 10s prompt timeouts.
    let deadline = Instant::now() + Duration::from_secs(if start { 4 } else { 8 });
    let result = (|| -> Result<Option<Value>, Error> {
        let input = hook_io::read_stdin(1024 * 1024, Instant::now() + Duration::from_secs(1))?;
        let input: Input = serde_json::from_slice(&input)?;
        if input.hook_event_name
            != if start {
                "SessionStart"
            } else {
                "UserPromptSubmit"
            }
            || !input.cwd.is_absolute()
        {
            return Err("invalid chair hook input".into());
        }
        check_time(Some(deadline))?;
        let context = Context::new(globals, &input.cwd)?;
        check_time(Some(deadline))?;
        if start {
            if context.store().pending(now())?.is_none() {
                return Ok(None);
            }
            let accounts = crate::quota_live::registered_claude(&context.roots)?;
            let target = current(&accounts, &context.home)?;
            check_time(Some(deadline))?;
            let Some(handoff) = context
                .store()
                .consume(target.key(), &input.session_id, now())?
            else {
                return Ok(None);
            };
            let path = context.store().path(&handoff.session_id);
            let text = format!(
                "Chair account-switch checkpoint consumed. Source session: {}. Read checkpoint {} and its state/handoff file before acting. The checkpoint lists open Fabric lanes by ID. Inspect these IDs in the run store and collect their results; never re-run completed lanes. Preserve originating authority limits.",
                handoff.session_id.as_str(),
                path.display()
            );
            Ok(Some(
                json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":text}}),
            ))
        } else {
            let Some(value) = switch(&context, &input, None, false, true, Some(deadline))? else {
                return Ok(None);
            };
            let text = message(&value);
            if value["outcome"] != "prepared" {
                Ok(Some(json!({"continue":true,"systemMessage":text})))
            } else {
                Ok(Some(
                    json!({"continue":false,"stopReason":text,"systemMessage":text}),
                ))
            }
        }
    })();
    match result {
        Ok(Some(value)) => {
            let _ = hook_io::write_stdout(format!("{value}\n").as_bytes(), deadline);
        }
        Ok(None) => {}
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "chair switch unavailable: {}",
                error.to_string().chars().take(512).collect::<String>()
            );
        }
    }
    0
}

#[cfg(test)]
mod tests {
    // The hook timing test measures real monotonic time.
    #![allow(clippy::disallowed_methods)]

    #[test]
    fn chair_command_and_hook_run_the_same_in_every_shell() -> Result<(), super::Error> {
        use super::*;
        use provenant_core::{PathResolver, RootInputs};
        use std::os::unix::fs::PermissionsExt;
        struct Resolver;
        impl PathResolver for Resolver {
            fn resolve_existing_ancestor(&self, path: &Path) -> io::Result<PathBuf> {
                Ok(path.into())
            }
        }
        let tmp = tempfile::tempdir()?;
        let home = tmp.path().canonicalize()?;
        let cwd = home.join(r"a b/$x's\y");
        std::fs::create_dir_all(&cwd)?;
        let context = Context {
            roots: RootInputs::new(home.clone(), cwd.clone()).resolve(&Resolver)?,
            cwd: cwd.clone(),
            assets: home.join("assets"),
            home: home.clone(),
        };
        let target = RegisteredAccount::new("claude", "work", home.join(".claude-work"))?;
        let checkpoint = home.join("checkpoint.md");
        // The hook command carries this executable path, so it needs the same quoting.
        let executable = home.join(r"e'x\ec/provenant");
        std::fs::create_dir_all(executable.parent().ok_or("executable directory")?)?;
        std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\"\n")?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))?;
        // A fake claude prints its working directory, config and state variables, and argv.
        let bin = home.join("bin");
        std::fs::create_dir(&bin)?;
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$CLAUDE_CONFIG_DIR\" \"$PROVENANT_STATE_DIR\" \"$@\"\n",
        )?;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o700))?;
        let command = command_with_executable(&context, &target, &checkpoint, &executable)?;
        let env = vec![(
            OsString::from("PATH"),
            OsString::from(format!("{}:/usr/bin:/bin", bin.display())),
        )];
        // Each shell stands in for the user's login shell, which parses the pane
        // command, and the hook command, which Claude runs through a shell.
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            if shell != "/bin/sh" && !Path::new(shell).is_file() {
                continue;
            }
            let argv = vec![shell.into(), "-c".into(), command.clone().into()];
            let mut options = ExecOptions::new(&argv, &home, Duration::from_secs(2));
            options.env = Some(env.as_slice());
            options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
            let output = SystemExecutor::new(1)?.execute(options)?;
            assert_eq!(output.termination, Termination::Exit(0));
            let output = std::str::from_utf8(&output.stdout.bytes)?;
            let mut lines = output.lines();
            assert_eq!(lines.next(), cwd.to_str());
            assert_eq!(lines.next(), target.directory().to_str());
            assert_eq!(lines.next(), context.roots.state.as_path().to_str());
            assert_eq!(lines.next(), Some("--settings"));
            let settings: Value = serde_json::from_str(lines.next().ok_or("missing settings")?)?;
            let hook = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
                .as_str()
                .ok_or("missing hook command")?;
            assert_eq!(
                hook,
                format!(
                    "{} hook claude chair-start",
                    super::quote(executable.to_str().ok_or("executable")?)
                )
            );
            let hook_argv = vec![shell.into(), "-c".into(), hook.into()];
            let mut hook_options = ExecOptions::new(&hook_argv, &home, Duration::from_secs(2));
            hook_options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
            let hook_output = SystemExecutor::new(1)?.execute(hook_options)?;
            assert_eq!(hook_output.termination, Termination::Exit(0));
            assert_eq!(
                std::str::from_utf8(&hook_output.stdout.bytes)?,
                format!("{}\nhook\nclaude\nchair-start\n", executable.display())
            );
            assert!(
                lines
                    .next()
                    .is_some_and(|prompt| prompt.contains("checkpoint.md"))
            );
        }
        assert!(
            command_with_executable(&context, &target, &checkpoint, Path::new("/a\nb/provenant"))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn chair_quote_encodes_apostrophes_and_backslashes() {
        assert_eq!(super::quote("/a b/$x"), "'/a b/$x'");
        assert_eq!(super::quote("/a'b"), "'/a'\"'\"'b'");
        assert_eq!(super::quote("'a''"), "\"'\"'a'\"'\"\"'\"");
        assert_eq!(super::quote(r"/a\b"), r#"'/a'"\\"'b'"#);
        assert_eq!(super::quote(r"\"), r#""\\""#);
        assert_eq!(super::quote(""), "''");
    }

    #[test]
    fn chair_launch_paths_require_utf8() {
        use super::*;
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));
        assert!(terminal_path(&path).is_err());
    }
    #[test]
    fn chair_hook_reserves_time_before_snapshot_and_launch() {
        use super::*;
        let instant = Instant::now();
        assert!(check_time(None).is_ok());
        assert!(check_time(Some(instant + Duration::from_secs(8))).is_ok());
        assert!(check_time(Some(instant + Duration::from_secs(2))).is_err());
        assert!(check_time(Some(instant)).is_err());
    }
}
