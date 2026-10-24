//! Opt-in chair handoff composition. Hooks read cached quota, never probe it.
use provenant_cli::{Chair, Globals};
use provenant_core::Roots;
use provenant_providers::identity::RegisteredAccount;
use provenant_quota::{
    AccountRecord, QuotaSnapshot,
    chair::{SwitchDecision, decide_switch},
};
use provenant_records::{CommandResult, Outcome};
use provenant_supervisor::{
    exec::{ExecOptions, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::{Directory, LocalFs},
    sys::hook_io,
};
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
    #[serde(default)]
    prompt: Option<String>,
}
struct Context {
    roots: Roots,
    cwd: PathBuf,
    assets: PathBuf,
    home: PathBuf,
}
impl Context {
    fn new(globals: &Globals, cwd: &Path) -> Result<Self, Error> {
        let cwd = std::fs::canonicalize(cwd)?;
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
    record.snapshot
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn command(
    context: &Context,
    target: &RegisteredAccount,
    checkpoint: &Path,
) -> Result<String, Error> {
    // Supply these hooks to the new session even when the target has only its
    // own credentials/settings. No config directory contents are copied.
    let executable = std::env::current_exe()?;
    let hook = |leaf: &str| -> Result<String, Error> {
        Ok(format!(
            "{} hook claude {leaf}",
            quote(executable.to_str().ok_or("executable path is not UTF-8")?)
        ))
    };
    let settings = json!({"hooks":{
        "SessionStart":[{"hooks":[{"type":"command","command":hook("chair-start")?,"timeout":5}]}],
        "UserPromptSubmit":[{"hooks":[{"type":"command","command":hook("chair-prompt")?,"timeout":10}]}]
    }});
    let prompt = format!(
        "Continue the chair handoff from checkpoint {}. Read it and its state/handoff file first. Inspect the listed Fabric lane IDs in the run store before acting; collect existing results and never re-run completed lanes. Preserve the originating authority limits.",
        checkpoint.display()
    );
    Ok(format!(
        "cd {} && CLAUDE_CONFIG_DIR={} PROVENANT_STATE_DIR={} claude --settings {} {}",
        quote(context.cwd.to_str().ok_or("cwd is not UTF-8")?),
        quote(
            target
                .directory()
                .to_str()
                .ok_or("account directory is not UTF-8")?
        ),
        quote(
            context
                .roots
                .state
                .as_path()
                .to_str()
                .ok_or("state directory is not UTF-8")?
        ),
        quote(&settings.to_string()),
        quote(&prompt)
    ))
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
fn launch(context: &Context, command: &str) -> Result<String, Error> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1")
        || std::env::var_os("HERDR_PANE_ID").is_none()
    {
        return Err("not running in a Herdr-managed pane".into());
    }
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
    execute_herdr(&["pane".into(), "run".into(), pane.into(), command.into()])?;
    Ok(pane.into())
}
fn switch(
    context: &Context,
    input: &Input,
    to: Option<&str>,
    dry_run: bool,
    automatic: bool,
) -> Result<Option<Value>, Error> {
    if !dry_run && let Some(previous) = context.store().prepared(&input.session_id)? {
        return Ok(Some(
            json!({"dry_run":false,"target_account":previous.target_account,"checkpoint":context.store().path(&input.session_id),"already_prepared":true,"launch_error":"handoff already prepared for this session; do not launch again","command":null}),
        ));
    }
    let config = provenant_config::read_with_domains(
        &context.roots,
        &LocalFs,
        &provenant_cli::domains::SECTIONS,
    )?;
    let knob = |name: &str| config.knobs.get(name).map(|k| &k.value);
    if automatic && knob("chair.auto_switch").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let threshold = knob("chair.switch_threshold")
        .and_then(Value::as_u64)
        .unwrap_or(92);
    let allowed = knob("chair.switch_accounts")
        .and_then(Value::as_array)
        .ok_or("invalid switch allow-list")?;
    let accounts = crate::quota_live::registered_claude()?;
    let current = current(&accounts, &context.home)?;
    let snapshot = cached(&context.roots, current.key());
    let targets = allowed
        .iter()
        .filter_map(Value::as_str)
        .filter(|label| *label != current.key() && to.is_none_or(|to| to == *label))
        .filter_map(|label| accounts.iter().find(|a| a.key() == label))
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
    let decision = decide_switch(snapshot.as_ref(), &choices, threshold, now(), automatic);
    let (target, reason) = match decision {
        SwitchDecision::Stay => return Ok(None),
        SwitchDecision::Switch(label) => (
            targets.iter().find(|a| a.key() == label).copied(),
            "quota-aware chair handoff",
        ),
        SwitchDecision::Refuse(reason) => (None, reason),
    };
    let store = context.store();
    let path = store.path(&input.session_id);
    let mut handoff = Handoff {
        session_id: input.session_id.clone(),
        cwd: context.cwd.clone(),
        transcript_path: input.transcript_path.clone(),
        state_handoff_file: None,
        next_step: input
            .prompt
            .as_ref()
            .map(|text| text.chars().take(4000).collect()),
        open_lanes: vec![],
        target_account: target.map(|a| a.key().into()),
        reason: reason.into(),
        consumed: false,
    };
    if dry_run {
        return Ok(Some(
            json!({"dry_run":true,"target_account":handoff.target_account,"reason":reason,"checkpoint":path,"command":target.map(|a| command(context,a,&path)).transpose()?}),
        ));
    }
    let (project, lanes) =
        provenant_cli::run::chair_snapshot(&context.edge()).map_err(|e| e.message)?;
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
    let launch_command = target.map(|a| command(context, a, &path)).transpose()?;
    let (path, fresh) = store.save(&handoff)?;
    let Some(target) = target else {
        return Ok(Some(
            json!({"refused":true,"reason":reason,"checkpoint":path,"dry_run":false}),
        ));
    };
    let command = launch_command.ok_or("launch command unavailable")?;
    let (pane, launch_error) = if fresh {
        match launch(context, &command) {
            Ok(pane) => (Some(pane), None),
            Err(error) => (None, Some(error.to_string())),
        }
    } else {
        (
            None,
            Some("handoff already prepared for this session; do not launch again".into()),
        )
    };
    Ok(Some(
        json!({"dry_run":false,"target_account":target.key(),"checkpoint":path,"command":command,"pane":pane,"launch_error":launch_error,"already_prepared":!fresh}),
    ))
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
        let id = session_id
            .or_else(|| std::env::var("CLAUDE_SESSION_ID").ok())
            .unwrap_or(format!(
                "manual-{}",
                provenant_supervisor::fs::temporary_name()?.to_string_lossy()
            ));
        let input = Input {
            hook_event_name: "manual".into(),
            session_id: HostSessionId::parse(&id)?,
            cwd: context.cwd.clone(),
            transcript_path: None,
            prompt: None,
        };
        switch(&context, &input, to.as_deref(), dry_run, false)?
            .ok_or_else(|| "no switch decision".into())
    })();
    match result {
        Ok(value) => {
            let refused = value.get("refused").and_then(Value::as_bool) == Some(true)
                || value.get("target_account") == Some(&Value::Null);
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
            Outcome::Error,
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
    if value["already_prepared"] == true {
        return format!(
            "Chair handoff already prepared. Stop this session; do not launch again. Checkpoint: {}",
            value["checkpoint"].as_str().unwrap_or("unavailable")
        );
    }
    if value.get("refused").and_then(Value::as_bool) == Some(true)
        || value.get("target_account") == Some(&Value::Null)
    {
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
pub fn hook(start: bool, globals: &Globals) -> u8 {
    let deadline = Instant::now() + Duration::from_secs(8);
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
        let context = Context::new(globals, &input.cwd)?;
        if start {
            if context.store().pending()?.is_none() {
                return Ok(None);
            }
            let accounts = crate::quota_live::registered_claude()?;
            let target = current(&accounts, &context.home)?;
            let Some(handoff) = context.store().consume(target.key(), &input.session_id)? else {
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
            let Some(value) = switch(&context, &input, None, false, true)? else {
                return Ok(None);
            };
            let text = message(&value);
            if value.get("refused").and_then(Value::as_bool) == Some(true) {
                Ok(Some(json!({"systemMessage":text})))
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
