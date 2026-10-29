//! Opt-in chair handoff composition. Hooks read cached quota, never probe it.
use provenant_cli::{Chair, ChairLog, ChairSummary, ChairViewer, Globals};
use provenant_core::{Clock, Roots};
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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Error = Box<dyn std::error::Error>;
#[derive(Debug, thiserror::Error)]
#[error(
    "viewer baselines cannot be changed inside a lane; only seen, snoozed and dismissed request presentation state is allowed"
)]
struct ViewerLane;
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
    /// The session's `CLAUDE_CONFIG_DIR`, if the environment names one.
    claude_config: Option<PathBuf>,
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
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::At(&cwd),
            crate::roots::Product::Required,
        )?;
        Ok(Self {
            roots: resolved.roots,
            cwd,
            assets: resolved.assets,
            home: resolved.home,
            claude_config: resolved.claude_config,
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
    context: &Context,
) -> Result<&'a RegisteredAccount, Error> {
    let dir = context
        .claude_config
        .clone()
        .unwrap_or_else(|| context.home.join(".claude"));
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
    let accounts = crate::registered_claude(&context.roots)?;
    let current = current(&accounts, context)?;
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
        let accounts = crate::registered_claude(&context.roots)?;
        let target = accounts
            .iter()
            .find(|a| a.key() == label)
            .ok_or("handoff target is no longer registered")?;
        command(context, target, &path)
    })();
    json!({"outcome":"already_prepared","dry_run":false,"target_account":handoff.target_account,"checkpoint":path,"command":recovery.as_ref().ok(),"launch_error":recovery.as_ref().err().map(|e|e.to_string())})
}
pub fn run(chair: Chair, globals: &Globals) -> u8 {
    let chair = match chair {
        Chair::Log(log) => return log_run(log, globals),
        Chair::Summary(summary) => return summary_run(summary, globals),
        Chair::Viewer(viewer) => return viewer_run(viewer, globals),
        chair => chair,
    };
    let Chair::Switch {
        to,
        dry_run,
        session_id,
    } = chair
    else {
        unreachable!()
    };
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
fn viewer_run(command: ChairViewer, globals: &Globals) -> u8 {
    use provenant_fabric_store::{Request, RequestView, Store, ThreadWait};
    let path = "chair viewer";
    let read_only = matches!(
        command,
        ChairViewer::Request { .. } | ChairViewer::Summary { .. }
    );
    let result = (|| -> Result<Value, Error> {
        if !read_only
            && !matches!(&command, ChairViewer::SetRequest { .. })
            && provenant_cli::fabric::in_lane()
        {
            return Err(ViewerLane.into());
        }
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        )?;
        let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
        let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
        let executor = SystemExecutor::new(1)?;
        let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
        let location = runtime
            .project_location(
                &executor,
                &resolved.project,
                budget,
                &provenant_core::CancelToken::new(),
            )?
            .location;
        let project = provenant_config::lookup_project(&resolved.roots, &location)?
            .ok_or("project not registered")?
            .project_id();
        let clock = provenant_os::clock::SystemClock;
        let cancel = provenant_core::CancelToken::new();
        let request = Request {
            clock: &clock,
            budget: provenant_core::WaitBudget::cli(clock.mono_now(), 5)?,
            cancel: &cancel,
            backoff: &ThreadWait,
        };
        let store = if read_only {
            match Store::open_existing(&resolved.roots.state, &request) {
                Ok(store) => Some(store),
                Err(provenant_fabric_store::Error::NotFound) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            Some(Store::open(&resolved.roots.state, &request)?)
        };
        match command {
            ChairViewer::Request { viewer, id } => {
                let store = store
                    .as_ref()
                    .ok_or(provenant_fabric_store::Error::NotFound)?;
                let item = store.human_request(project, id, &request)?;
                Ok(
                    json!({"request": item, "view": store.request_view(project, &viewer, id, &request)?}),
                )
            }
            ChairViewer::SetRequest {
                viewer,
                id,
                seen_revision,
                snoozed_until,
                dismissed,
            } => {
                let store = store
                    .as_ref()
                    .ok_or(provenant_fabric_store::Error::NotFound)?;
                let value = RequestView {
                    seen_revision,
                    snoozed_until: snoozed_until
                        .map(|s| provenant_core::WallTime::parse(&s))
                        .transpose()?,
                    dismissed,
                };
                store.set_request_view(project, &viewer, id, &value, &request)?;
                Ok(json!({"view": store.request_view(project, &viewer, id, &request)?}))
            }
            ChairViewer::Summary {
                viewer,
                session,
                to,
            } => {
                if viewer.is_empty() || viewer.len() > 128 || viewer.chars().any(char::is_control) {
                    return Err(provenant_fabric_store::Error::Invalid("viewer").into());
                }
                let baseline = store
                    .as_ref()
                    .map(|s| s.summary_baseline(project, &viewer, &session, &request))
                    .transpose()?
                    .flatten();
                let workflow = SessionStore::new(
                    resolved.roots.state.clone(),
                    project,
                    HostSessionId::parse(&session)?,
                );
                if baseline.zip(to).is_some_and(|(from, to)| to < from) {
                    return Err(provenant_fabric_store::Error::Conflict.into());
                }
                let current = workflow
                    .read()?
                    .map(|c| {
                        provenant_workflow::session::parse_summary(&c.markdown)
                            .map_err(provenant_workflow::session::SessionError::Summary)
                    })
                    .transpose()?;
                let summary = current.flatten();
                let Some(summary) = summary else {
                    return Ok(
                        json!({"baseline":baseline,"to":to,"summary":null,"coverage":"unavailable","diff":baseline.map(|n| json!({"from":n,"to":to,"unavailable":[n],"now":null,"next":null,"needs_you":null,"risk":null}))}),
                    );
                };
                let to = to.unwrap_or(summary.revision);
                if baseline.is_some_and(|from| to < from) {
                    return Ok(
                        json!({"baseline":baseline,"to":to,"summary":summary,"coverage":"baseline_ahead","diff":null}),
                    );
                }
                if to > summary.revision {
                    return Err(provenant_fabric_store::Error::Invalid("summary revision").into());
                }
                let (diff, coverage) = match workflow.viewer_summary_diff(baseline, Some(to)) {
                    Ok(diff) => (serde_json::to_value(diff)?, "available"),
                    Err(
                        provenant_workflow::session::SessionError::MissingCheckpoint { .. }
                        | provenant_workflow::session::SessionError::Summary(_),
                    ) => (json!(baseline.map(|n| json!({"from":n,"to":to,"unavailable":[n],"now":null,"next":null,"needs_you":null,"risk":null}))), "unavailable"),
                    Err(error) => return Err(error.into()),
                };
                let rendered = (to == summary.revision).then_some(&summary);
                Ok(
                    json!({"baseline": baseline, "to":to, "summary":rendered, "coverage":coverage, "diff":diff}),
                )
            }
            ChairViewer::Rendered {
                viewer,
                session,
                revision,
            } => {
                let store = store
                    .as_ref()
                    .ok_or(provenant_fabric_store::Error::NotFound)?;
                let workflow = SessionStore::new(
                    resolved.roots.state.clone(),
                    project,
                    HostSessionId::parse(&session)?,
                );
                let checkpoint = workflow.read()?.ok_or("checkpoint unavailable")?;
                let summary = provenant_workflow::session::parse_summary(&checkpoint.markdown)?
                    .ok_or("summary unavailable")?;
                if revision == 0 || revision > summary.revision {
                    return Err(provenant_fabric_store::Error::Invalid("summary revision").into());
                }
                store.set_summary_baseline(project, &viewer, &session, revision, &request)?;
                Ok(json!({"baseline": revision}))
            }
            ChairViewer::Forget { viewer, session } => {
                HostSessionId::parse(&session)?;
                store
                    .as_ref()
                    .ok_or(provenant_fabric_store::Error::NotFound)?
                    .forget_summary_baseline(project, &viewer, &session, &request)?;
                Ok(json!({"baseline": null}))
            }
        }
    })();
    match result {
        Ok(value) => {
            let human = serde_json::to_string_pretty(&value).unwrap_or_default();
            crate::emit(
                &CommandResult::new(path, Outcome::Ok, value),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => {
            use provenant_fabric_store::Error as E;
            let (code, repair) = match error.downcast_ref::<E>() {
                Some(E::SchemaVersion(_) | E::UnversionedDatabase) => (
                    "VIEWER_INCOMPATIBLE",
                    "Open the store with a normal inbox command using a compatible release, then retry.",
                ),
                Some(E::ViewerLimit) => (
                    "VIEWER_LIMIT",
                    "Forget an unused baseline with chair viewer forget VIEWER SESSION, or close requests and reclaim their viewer state, then retry.",
                ),
                Some(E::ViewerNotOpen) => {
                    ("VIEWER_NOT_OPEN", "Refresh the request before retrying.")
                }
                Some(E::NotFound) => (
                    "VIEWER_NOT_FOUND",
                    "Inspect the request or session and retry.",
                ),
                Some(E::Conflict) => (
                    "VIEWER_CONFLICT",
                    "Use chair viewer forget VIEWER SESSION to reset the baseline, then retry.",
                ),
                Some(E::Busy | E::Cancelled) => ("VIEWER_RETRY", "Retry the operation."),
                Some(E::Invalid(_)) => (
                    "VIEWER_INVALID",
                    "Correct the viewer name or revision and retry.",
                ),
                _ if error.is::<ViewerLane>() => (
                    "VIEWER_LANE",
                    "Change summary baselines outside the lane; request presentation state may change inside one.",
                ),
                _ if error.is::<provenant_workflow::session::HostSessionIdError>() => {
                    ("VIEWER_INVALID", "Correct the session ID and retry.")
                }
                _ if error.is::<provenant_workflow::session::SessionError>() => {
                    ("VIEWER_SESSION", "Inspect the session and retry.")
                }
                _ => ("VIEWER_UNAVAILABLE", "Inspect the session and retry."),
            };
            crate::fail_with_repair(
                path,
                Outcome::Refused,
                code,
                &error.to_string(),
                Some(repair),
                globals,
            )
        }
    }
}
#[derive(Deserialize, Serialize)]
struct LogInput {
    kind: String,
    markdown: String,
    channel: String,
    #[serde(default)]
    request_ids: Vec<String>,
    #[serde(default)]
    attempt_ids: Vec<String>,
    #[serde(default)]
    issue_ids: Vec<String>,
    #[serde(default)]
    pr_ids: Vec<String>,
    #[serde(default)]
    correction_of: Option<u64>,
}

// The standard input transport has a real deadline.
#[allow(clippy::disallowed_methods)]
fn log_input<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, Error> {
    Ok(serde_json::from_slice(&log_bytes(path)?)?)
}

fn log_request(path: &str) -> Result<(LogInput, Vec<provenant_records::InputWarning>), Error> {
    Ok(provenant_records::decode_input(path, &log_bytes(path)?)?)
}

#[allow(clippy::disallowed_methods)]
fn log_bytes(path: &str) -> Result<Vec<u8>, Error> {
    const LIMIT: usize = 256 * 1024;
    let bytes = if path == "-" {
        hook_io::read_stdin(LIMIT, Instant::now() + Duration::from_secs(30))?
    } else {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > LIMIT {
            return Err("input exceeds 256 KiB".into());
        }
        bytes
    };
    Ok(bytes)
}

fn summary_run(command: ChairSummary, globals: &Globals) -> u8 {
    use provenant_workflow::session::{SessionError, SummaryPatch, parse_summary};
    let read_only = !matches!(command, ChairSummary::Update { .. });
    let path = match &command {
        ChairSummary::Show { .. } => "chair summary show",
        ChairSummary::Diff { .. } => "chair summary diff",
        ChairSummary::Update { .. } => "chair summary update",
    };
    let result = (|| -> Result<Value, Error> {
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        )?;
        match command {
            ChairSummary::Show { ref session } | ChairSummary::Diff { ref session, .. } => {
                let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
                let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
                let executor = SystemExecutor::new(1)?;
                let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
                let location = runtime
                    .project_location(
                        &executor,
                        &resolved.project,
                        budget,
                        &provenant_core::CancelToken::new(),
                    )?
                    .location;
                let project = provenant_config::lookup_project(&resolved.roots, &location)?
                    .ok_or("project not registered")?
                    .project_id();
                let store = SessionStore::new(
                    resolved.roots.state.clone(),
                    project,
                    HostSessionId::parse(session)?,
                );
                if let ChairSummary::Diff { from, to, .. } = command {
                    Ok(json!({"diff": store.summary_diff(from, to)?}))
                } else {
                    let checkpoint =
                        store
                            .read()?
                            .ok_or_else(|| SessionError::MissingCheckpoint {
                                session: session.clone(),
                            })?;
                    let summary = parse_summary(&checkpoint.markdown)?;
                    let comparison = summary.as_ref().map(|value| {
                        provenant_cli::console::summary_comparison(
                            &resolved.roots,
                            project,
                            session,
                            &value.needs_you,
                        )
                    });
                    Ok(json!({"summary": summary, "comparison": comparison}))
                }
            }
            ChairSummary::Update {
                session,
                file,
                expect_revision,
                operation_id,
            } => {
                let operation_id = match operation_id {
                    Some(id) => id,
                    None => {
                        let bytes = provenant_os::fs::entropy_128()?;
                        provenant_core::OperationId::parse(&format!(
                            "cli-{}",
                            provenant_core::AttemptId::from_bytes(bytes)
                        ))?
                    }
                };
                let patch: SummaryPatch = log_input(&file)?;
                let context = provenant_cli::fabric::Context::open(
                    resolved.roots,
                    &resolved.home,
                    &resolved.project,
                )?;
                if context.in_lane {
                    return Err("a lane cannot update a chair summary".into());
                }
                let actor = provenant_cli::workflow::session_actor(&context, None)?;
                let keep = provenant_cli::workflow::session_revisions(&context.roots);
                let clock = provenant_os::clock::SystemClock::new();
                let budget = provenant_core::WaitBudget::cli(clock.mono_now(), 5)?;
                let store = SessionStore::new(
                    context.roots.state,
                    context.identity.project,
                    HostSessionId::parse(&session)?,
                )
                .with_chair(context.identity.host, actor)
                .with_revisions(keep);
                store.validate_chair()?;
                let outcome = store.update_summary(
                    patch,
                    expect_revision,
                    &operation_id,
                    budget,
                    &provenant_core::CancelToken::new(),
                    &clock,
                )?;
                Ok(json!({
                    "summary": outcome.summary,
                    "checkpoint": outcome.saved,
                    "operation_id": operation_id,
                    "replayed": outcome.replayed,
                }))
            }
        }
    })();
    match result {
        Ok(value) => {
            let human = serde_json::to_string_pretty(&value).unwrap_or_default();
            crate::emit(
                &CommandResult::new(path, Outcome::Ok, value),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => {
            if let Some(SessionError::SummaryStale { current_revision }) =
                error.downcast_ref::<SessionError>()
            {
                let mut result = CommandResult::new(
                    path,
                    Outcome::Refused,
                    json!({"current_revision": current_revision}),
                );
                result.error = Some(provenant_records::CommandError {
                    code: "SUMMARY_STALE".into(),
                    message: error.to_string(),
                    cause: None,
                    repair: Some("Reread with `provenant chair summary show` and reapply the intended change.".into()),
                });
                crate::emit(&result, globals, "", false)
            } else if matches!(
                error.downcast_ref::<SessionError>(),
                Some(SessionError::MissingCheckpoint { .. })
            ) {
                crate::fail(
                    path,
                    Outcome::Failed,
                    "MISSING",
                    &error.to_string(),
                    globals,
                )
            } else {
                crate::fail(
                    path,
                    Outcome::Refused,
                    "CHAIR_SUMMARY",
                    &error.to_string(),
                    globals,
                )
            }
        }
    }
}

fn log_run(command: ChairLog, globals: &Globals) -> u8 {
    use provenant_workflow::log::{Append, LogStore, Retention};
    let read_only = matches!(command, ChairLog::List { .. });
    let path = if read_only {
        "chair log list"
    } else {
        "chair log append"
    };
    let result = (|| -> Result<Value, Error> {
        let resolved = crate::roots::resolve(
            globals,
            crate::roots::Workspace::Project,
            crate::roots::Product::Required,
        )?;
        match command {
            ChairLog::Append {
                session,
                file,
                operation_id,
                issued_at,
            } => {
                let created_at = now();
                let operation_id = match operation_id {
                    Some(id) => id,
                    None => {
                        let bytes = provenant_os::fs::entropy_128()?;
                        provenant_core::OperationId::parse(&format!(
                            "cli-{}",
                            provenant_core::AttemptId::from_bytes(bytes)
                        ))?
                    }
                };
                let issued_at = issued_at.unwrap_or(created_at);
                let context = provenant_cli::fabric::Context::open(
                    resolved.roots,
                    &resolved.home,
                    &resolved.project,
                )?;
                if context.in_lane {
                    return Err("a lane cannot append a chair log".into());
                }
                let (input, warnings) = log_request(&file)?;
                let config = provenant_config::read_with_domains(
                    &context.roots,
                    &provenant_cli::domains::SECTIONS,
                )?;
                let settings = config.retention;
                let retention = Retention {
                    max_age_seconds: settings.chair_log_days * 86400,
                    replay_horizon_seconds: settings.chair_log_replay_seconds(),
                    max_entries: settings.chair_log_entries,
                    max_bytes: settings.chair_log_bytes,
                };
                let entry = LogStore::new(
                    context.roots.state,
                    context.identity.project,
                    HostSessionId::parse(&session)?,
                )
                .append(
                    Append {
                        operation_id,
                        issued_at,
                        host: context.identity.host.to_string(),
                        kind: input.kind,
                        markdown: input.markdown,
                        request_ids: input.request_ids,
                        attempt_ids: input.attempt_ids,
                        issue_ids: input.issue_ids,
                        pr_ids: input.pr_ids,
                        channel: input.channel,
                        correction_of: input.correction_of,
                    },
                    retention,
                    created_at,
                )?;
                let mut data = serde_json::to_value(entry)?;
                if !warnings.is_empty() {
                    data["warnings"] =
                        json!(warnings.iter().map(ToString::to_string).collect::<Vec<_>>());
                }
                Ok(data)
            }
            ChairLog::List {
                session,
                after,
                limit,
            } => {
                let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
                let runtime = provenant_confinement::UtilityRuntime::read_only(git)?;
                let executor = SystemExecutor::new(1)?;
                let budget = provenant_confinement::UtilityBudget::bounded(Duration::from_secs(5))?;
                let location = runtime
                    .project_location(
                        &executor,
                        &resolved.project,
                        budget,
                        &provenant_core::CancelToken::new(),
                    )?
                    .location;
                let project = provenant_config::lookup_project(&resolved.roots, &location)?
                    .ok_or("project not registered")?
                    .project_id();
                let settings = provenant_config::read_with_domains(
                    &resolved.roots,
                    &provenant_cli::domains::SECTIONS,
                )?
                .retention;
                let retention = Retention {
                    max_age_seconds: settings.chair_log_days * 86400,
                    replay_horizon_seconds: settings.chair_log_replay_seconds(),
                    max_entries: settings.chair_log_entries,
                    max_bytes: settings.chair_log_bytes,
                };
                let page = LogStore::new(
                    resolved.roots.state,
                    project,
                    HostSessionId::parse(&session)?,
                )
                .page(after, limit as usize, retention, now())?;
                Ok(serde_json::to_value(page)?)
            }
        }
    })();
    match result {
        Ok(value) => {
            let human = serde_json::to_string_pretty(&value).unwrap_or_default();
            crate::emit(
                &CommandResult::new(path, Outcome::Ok, value),
                globals,
                &human,
                read_only,
            )
        }
        Err(error) => {
            let code = match error.downcast_ref::<provenant_workflow::log::Error>() {
                Some(provenant_workflow::log::Error::Expired) => "CHAIR_LOG_EXPIRED",
                Some(provenant_workflow::log::Error::Future) => "CHAIR_LOG_FUTURE",
                _ => "CHAIR_LOG",
            };
            crate::fail(path, Outcome::Refused, code, &error.to_string(), globals)
        }
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
/// The session's Claude config directory relative to home, so a report can find
/// its transcript even when no registered account owns the directory.
fn config_hint(context: &Context) -> Option<String> {
    let dir = context
        .claude_config
        .clone()
        .unwrap_or_else(|| context.home.join(".claude"));
    let relative = std::fs::canonicalize(dir)
        .ok()?
        .strip_prefix(std::fs::canonicalize(&context.home).ok()?)
        .ok()?
        .to_owned();
    let text = relative.to_str()?;
    (!text.is_empty()
        && text.len() <= 64
        && relative
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))))
    .then(|| text.to_owned())
}
// Attribution is best effort and must not prevent consuming a handoff. A
// session whose config directory is not a registered account (the default
// `~/.claude` included) is still recorded, as `unattributed`: launching the
// chair with `CLAUDE_CONFIG_DIR` set to a registered account's directory is
// what attributes it.
fn record_start(context: &Context, input: &Input) {
    let account = crate::registered_claude(&context.roots)
        .ok()
        .and_then(|accounts| current(&accounts, context).ok().map(|a| a.key().to_owned()));
    let project = if crate::usage::attribution_enabled(&context.roots) {
        crate::hooks::with_git(|runtime, executor, budget, cancel| {
            let location = runtime
                .project_location(executor, &context.cwd, budget, cancel)?
                .location;
            Ok(provenant_config::lookup_project(&context.roots, &location)?
                .map(|binding| binding.project_id()))
        })
        .ok()
        .flatten()
    } else {
        None
    };
    provenant_telemetry::usage::record_chair(
        &context.roots,
        &provenant_os::clock::SystemClock::new(),
        account
            .as_deref()
            .unwrap_or(provenant_telemetry::usage::UNATTRIBUTED),
        input.session_id.as_str(),
        project,
        config_hint(context).as_deref(),
    );
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
            // Consume any handoff first, so attribution never spends its deadline.
            let handoff = if context.store().pending(now())?.is_some() {
                let accounts = crate::registered_claude(&context.roots)?;
                let target = current(&accounts, &context)?;
                check_time(Some(deadline))?;
                context
                    .store()
                    .consume(target.key(), &input.session_id, now())?
            } else {
                None
            };
            record_start(&context, &input);
            let Some(handoff) = handoff else {
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
    fn chair_log_request_warns_with_the_source_file_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.json");
        std::fs::write(
            &path,
            br#"{"kind":"update","markdown":"kept","channel":"chat","future":true}"#,
        )
        .unwrap();
        let (input, warnings) = super::log_request(path.to_str().unwrap()).unwrap();
        assert_eq!(input.markdown, "kept");
        assert_eq!(warnings[0].source, path.to_str().unwrap());
        assert_eq!(warnings[0].key, "future");
    }

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
            claude_config: None,
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
