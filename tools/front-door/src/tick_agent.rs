//! Platform seam for the per-user host tick scheduler, published by the install receipt.
use provenant_fabric_runtime::peer_agent::{self, AgentError, launchctl, systemctl, utf8, xml};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

pub const LABEL: &str = "dev.provenant.tick";
const BOOTSTRAP_ATTEMPTS: usize = 5;
const BOOTSTRAP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EnableAction {
    Reload,
    Enable,
    Restart,
}

const TIMER_ENABLE_UNCHANGED: &[EnableAction] = &[EnableAction::Enable];
const TIMER_ENABLE_CHANGED: &[EnableAction] = &[
    EnableAction::Reload,
    EnableAction::Enable,
    EnableAction::Restart,
];

fn timer_enable_plan(changed: bool) -> &'static [EnableAction] {
    if changed {
        TIMER_ENABLE_CHANGED
    } else {
        TIMER_ENABLE_UNCHANGED
    }
}

/// launchd's interval job may be idle after a successful tick; that is healthy.
#[derive(Debug, Serialize, Deserialize)]
pub struct Diagnosis {
    pub state: String,
    pub last_exit_reason: Option<String>,
    pub warning: Option<String>,
    pub repair: Option<String>,
}

impl Default for Diagnosis {
    fn default() -> Self {
        Self {
            state: "unknown".into(),
            last_exit_reason: None,
            warning: None,
            repair: None,
        }
    }
}

pub fn diagnosis(state: &Path, home: &Path) -> Diagnosis {
    if !cfg!(target_os = "macos") || !eligible(state, home) {
        return Diagnosis {
            state: "not_applicable".into(),
            ..Default::default()
        };
    }
    diagnosis_installed(home, launchctl)
}

fn diagnosis_installed(
    home: &Path,
    run: impl Fn(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Diagnosis {
    if !plist(home).exists() {
        return Diagnosis {
            state: "not_installed".into(),
            warning: Some("Tick agent is not installed".into()),
            repair: Some("provenant upgrade".into()),
            ..Default::default()
        };
    }
    let service = format!("gui/{}/{LABEL}", provenant_os::fs::effective_uid());
    match run(&["print".as_ref(), service.as_ref()], home) {
        Ok((true, output)) => parse_diagnosis(&output),
        _ => Diagnosis {
            state: "unavailable".into(),
            warning: Some("Tick agent is not loaded or launchctl could not inspect it".into()),
            repair: Some("provenant upgrade".into()),
            ..Default::default()
        },
    }
}

/// Interpret launchd's textual protocol once at the platform boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TickExitCause {
    ExitCode,
    Signal,
    LaunchConstraint,
    Crash,
    Killed,
}

fn exit_cause(
    exit: Option<&str>,
    signal: Option<&str>,
    reason: Option<&str>,
) -> Option<TickExitCause> {
    if exit.is_some_and(|code| code != "0" && code != "(never exited)") {
        return Some(TickExitCause::ExitCode);
    }
    if signal.is_some_and(|signal| signal != "0") {
        return Some(TickExitCause::Signal);
    }
    let reason = reason?.to_ascii_lowercase();
    [
        ("constraint", TickExitCause::LaunchConstraint),
        ("crash", TickExitCause::Crash),
        ("kill", TickExitCause::Killed),
    ]
    .into_iter()
    .find_map(|(word, cause)| reason.contains(word).then_some(cause))
}

fn parse_diagnosis(output: &str) -> Diagnosis {
    let field = |key: &str| {
        output.lines().find_map(|line| {
            let (name, value) = line.trim().split_once(" = ")?;
            (name == key).then(|| {
                value
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(256)
                    .collect::<String>()
            })
        })
    };
    let state = field("state").unwrap_or_else(|| "unknown".into());
    let exit = field("last exit code");
    let signal = field("last terminating signal");
    let reason = field("last exit reason");
    let cause = exit_cause(exit.as_deref(), signal.as_deref(), reason.as_deref());
    let last_exit_reason = match (exit, signal, reason) {
        (None, None, None) => None,
        (exit, signal, reason) => Some(
            [
                exit.map(|value| format!("exit code {value}")),
                signal.map(|value| format!("signal {value}")),
                reason,
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("; "),
        ),
    };
    Diagnosis {
        state,
        last_exit_reason,
        warning: cause.is_some().then(|| {
            "Tick failed or is crash-looping; a SIGKILL may indicate a launch-constraint kill"
                .into()
        }),
        repair: cause.is_some().then(|| "provenant upgrade".into()),
    }
}
const TIMER: &str = "provenant-tick.timer";
/// Seconds between ticks on both platforms.
const INTERVAL: u32 = 60;

/// Whether this platform has a tick scheduler: launchd on macOS, systemd on Linux.
pub fn supported() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

pub fn eligible(state: &Path, home: &Path) -> bool {
    supported() && peer_agent::owns_default_state(state, home)
}

pub fn plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/dev.provenant.tick.plist")
}

pub fn timer(home: &Path) -> PathBuf {
    timer_in(home, std::env::var_os("XDG_CONFIG_HOME"))
}

fn timer_in(home: &Path, config_home: Option<OsString>) -> PathBuf {
    provenant_config::user_dir_in(home, config_home).join(TIMER)
}

pub fn service(home: &Path) -> PathBuf {
    timer(home).with_file_name("provenant-tick.service")
}

pub fn render(program: &Path, state: &Path, search: &OsStr) -> Result<Vec<u8>, AgentError> {
    let program = xml(utf8(program)?);
    let stdout = xml(utf8(&state.join("tick/stdout.log"))?);
    let stderr = xml(utf8(&state.join("tick/stderr.log"))?);
    let state = xml(utf8(state)?);
    let search = xml(utf8(Path::new(search))?);
    Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>tick</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{search}</string>
    <key>PROVENANT_STATE_DIR</key>
    <string>{state}</string>
  </dict>
  <key>StartInterval</key>
  <integer>{INTERVAL}</integer>
  <key>RunAtLoad</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#).into_bytes())
}

/// A systemd double-quoted word. `%` always expands; `$` expands only where
/// `dollars` is set (command lines), so `Environment=` values stay exact.
fn systemd_quote(text: &str, dollars: bool) -> Result<String, AgentError> {
    if text.chars().any(char::is_control) {
        return Err(AgentError::Path(text.into()));
    }
    let text = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    Ok(format!(
        "\"{}\"",
        if dollars {
            text.replace('$', "$$")
        } else {
            text
        }
    ))
}

/// An unquoted `append:` log target, which systemd takes verbatim after specifier expansion.
fn systemd_append(path: &str) -> Result<String, AgentError> {
    if path.chars().any(char::is_control) {
        return Err(AgentError::Path(path.into()));
    }
    Ok(format!(
        "append:{}",
        path.replace('%', "%%").replace(' ', "\\x20")
    ))
}

/// The Linux timer: the first tick at activation, then one per interval after the last.
pub fn render_timer() -> Vec<u8> {
    format!(
        "[Unit]\nDescription=Provenant host tick timer\n\n[Timer]\nOnActiveSec=0\nOnUnitActiveSec={INTERVAL}s\nAccuracySec=1s\n\n[Install]\nWantedBy=timers.target\n"
    )
    .into_bytes()
}

/// The Linux one-shot service the timer activates.
pub fn render_service(program: &Path, state: &Path, search: &OsStr) -> Result<Vec<u8>, AgentError> {
    let exec = systemd_quote(utf8(program)?, true)?;
    let path = systemd_quote(&format!("PATH={}", utf8(Path::new(search))?), false)?;
    let state_env = systemd_quote(&format!("PROVENANT_STATE_DIR={}", utf8(state)?), false)?;
    let stdout = systemd_append(utf8(&state.join("tick/stdout.log"))?)?;
    let stderr = systemd_append(utf8(&state.join("tick/stderr.log"))?)?;
    Ok(format!(
        "[Unit]\nDescription=Provenant host tick\n\n[Service]\nType=oneshot\nKillMode=process\nExecStart={exec} tick\nEnvironment={path}\nEnvironment={state_env}\nStandardOutput={stdout}\nStandardError={stderr}\n"
    )
    .into_bytes())
}

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Installed {
    Started,
    Skipped,
    Failed { message: String },
}

pub fn install(state: &Path, home: &Path, changed: bool, enabled: bool) -> Installed {
    if !enabled || !eligible(state, home) {
        return Installed::Skipped;
    }
    let result = provenant_config::private_directory(&state.join("tick"), true)
        .map_err(|error| std::io::Error::other(error).into())
        .and_then(|_| {
            if cfg!(target_os = "linux") {
                enable_with(changed, |args| systemctl(args, home))
            } else {
                bootstrap_with(home, launchctl)
            }
        });
    match result {
        Ok(()) => Installed::Started,
        Err(error) => Installed::Failed {
            message: format!("{error}"),
        },
    }
}

/// Always boots the agent out and back in. launchd evaluates the launch constraint
/// against the code identity of the program it recorded at bootstrap; an upgrade replaces
/// the ad-hoc signed `provenant` behind the stable `~/.local/bin` path, so a service left
/// loaded across an upgrade is killed for a Launch Constraint Violation (#2469).
// LaunchAgent bootstrap retries are bounded real-time OS operations.
#[allow(clippy::disallowed_methods)]
fn bootstrap_with(
    home: &Path,
    run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    bootstrap_with_pause(home, run, std::thread::park_timeout)
}

fn bootstrap_with_pause(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
    mut pause: impl FnMut(std::time::Duration),
) -> Result<(), AgentError> {
    let domain = format!("gui/{}", provenant_os::fs::effective_uid());
    let service = format!("{domain}/{LABEL}");
    let loaded = run(&["print".as_ref(), service.as_ref()], home)?.0;
    if loaded {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    let path = plist(home);
    let mut last = String::new();
    // launchd can refuse bootstrap briefly while a booted-out service exits.
    for _ in 0..BOOTSTRAP_ATTEMPTS {
        let (ok, error) = run(
            &["bootstrap".as_ref(), domain.as_ref(), path.as_os_str()],
            home,
        )?;
        if ok {
            return Ok(());
        }
        last = error;
        pause(BOOTSTRAP_RETRY_DELAY);
    }
    Err(AgentError::Launchctl("bootstrap", last))
}

/// Enable and start the timer. `enable --now` is idempotent; changed units are
/// reloaded and the timer restarted so a new interval or command applies.
fn enable_with(
    changed: bool,
    mut run: impl FnMut(&[&OsStr]) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let mut step = |name: &'static str, args: &[&str]| {
        let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        match run(&args)? {
            (true, _) => Ok(()),
            (false, error) => Err(AgentError::Systemctl(name, error)),
        }
    };
    for action in timer_enable_plan(changed) {
        match action {
            EnableAction::Reload => step("daemon-reload", &["daemon-reload"])?,
            EnableAction::Enable => step("enable", &["enable", "--now", TIMER])?,
            EnableAction::Restart => step("restart", &["restart", TIMER])?,
        }
    }
    Ok(())
}

/// Disable and stop the timer when it is enabled or running; absent is already stopped.
fn disable_with(
    mut run: impl FnMut(&[&OsStr]) -> Result<(bool, String), AgentError>,
) -> Result<bool, AgentError> {
    let present = run(&["is-enabled".as_ref(), TIMER.as_ref()])?.0
        || run(&["is-active".as_ref(), TIMER.as_ref()])?.0;
    if present {
        let (ok, error) = run(&["disable".as_ref(), "--now".as_ref(), TIMER.as_ref()])?;
        if !ok {
            return Err(AgentError::Systemctl("disable", error));
        }
    }
    Ok(present)
}

pub fn stop(home: &Path) -> Result<bool, AgentError> {
    if cfg!(target_os = "linux") {
        disable_with(|args| systemctl(args, home))
    } else {
        stop_with(home, LABEL, launchctl)
    }
}

/// Cut-over needs the executing oneshot service stopped as well as its timer.
/// Disabling a systemd timer alone does not stop an already dispatched tick.
pub fn quiesce(home: &Path) -> Result<bool, AgentError> {
    if cfg!(target_os = "linux") {
        quiesce_timer_with(|args| systemctl(args, home))
    } else {
        stop(home)
    }
}

fn quiesce_timer_with(
    mut run: impl FnMut(&[&OsStr]) -> Result<(bool, String), AgentError>,
) -> Result<bool, AgentError> {
    use peer_agent::ServiceState;
    let units = [TIMER, "provenant-tick.service"];
    // The follow-up queries decide quiescence even if a stop reports failure.
    let _ = run(&["disable".as_ref(), "--now".as_ref(), TIMER.as_ref()]);
    let _ = run(&["stop".as_ref(), units[1].as_ref()]);
    for unit in units {
        let state = match run(&[
            "show".as_ref(),
            unit.as_ref(),
            "--property=LoadState".as_ref(),
            "--property=ActiveState".as_ref(),
        ]) {
            Ok((true, output)) => {
                let load = output
                    .lines()
                    .find_map(|line| line.strip_prefix("LoadState="));
                let active = output
                    .lines()
                    .find_map(|line| line.strip_prefix("ActiveState="));
                match (load, active) {
                    (Some("loaded" | "not-found"), Some("inactive" | "failed")) => {
                        ServiceState::Absent
                    }
                    (
                        Some("loaded"),
                        Some("active" | "activating" | "deactivating" | "reloading"),
                    ) => ServiceState::Running,
                    _ => ServiceState::Indeterminate,
                }
            }
            _ => ServiceState::Indeterminate,
        };
        if state != ServiceState::Absent {
            return Err(AgentError::Quiescence {
                service: unit.into(),
                state,
                command: format!(
                    "systemctl --user disable --now {TIMER} && systemctl --user stop {}",
                    units[1]
                ),
            });
        }
    }
    Ok(true)
}

fn stop_with(
    home: &Path,
    label: &str,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<bool, AgentError> {
    peer_agent::stop_service_with(home, label, &mut run)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn tick_diagnosis_is_not_applicable_for_ineligible_state() {
        let home = tempfile::tempdir().unwrap();
        let diagnosis = diagnosis(&home.path().join("custom-state"), home.path());
        assert_eq!(diagnosis.state, "not_applicable");
        assert!(diagnosis.warning.is_none());
        assert!(diagnosis.repair.is_none());
    }

    #[test]
    fn tick_diagnosis_distinguishes_not_installed_from_not_loaded() {
        let home = tempfile::tempdir().unwrap();
        let missing = diagnosis_installed(home.path(), |_, _| panic!("must not inspect launchd"));
        assert_eq!(missing.state, "not_installed");
        assert_eq!(missing.repair.as_deref(), Some("provenant upgrade"));
        std::fs::create_dir_all(plist(home.path()).parent().unwrap()).unwrap();
        std::fs::write(plist(home.path()), "plist").unwrap();
        let unloaded = diagnosis_installed(home.path(), |_, _| Ok((false, String::new())));
        assert_eq!(unloaded.state, "unavailable");
    }

    #[test]
    fn tick_diagnosis_distinguishes_idle_success_from_constraint_kills_and_crashes() {
        let idle = parse_diagnosis("state = not running\nlast exit code = 0\n");
        assert_eq!(idle.state, "not running");
        assert!(idle.warning.is_none());
        assert_eq!(idle.last_exit_reason.as_deref(), Some("exit code 0"));
        for output in [
            "state = spawn scheduled\nlast exit code = 1\n",
            "state = not running\nlast terminating signal = 9\n",
            "state = not running\nlast exit reason = Launch Constraint Violation\n",
        ] {
            let diagnosis = parse_diagnosis(output);
            assert!(diagnosis.warning.is_some());
            assert!(diagnosis.last_exit_reason.is_some());
            assert_eq!(diagnosis.repair.as_deref(), Some("provenant upgrade"));
        }
    }

    #[test]
    fn launchd_exit_cause_is_classified_at_the_boundary() {
        assert_eq!(exit_cause(Some("0"), None, None), None);
        assert_eq!(exit_cause(Some("(never exited)"), Some("0"), None), None);
        for (text, cause) in [
            (
                "Launch Constraint Violation",
                TickExitCause::LaunchConstraint,
            ),
            ("service crashed", TickExitCause::Crash),
            ("was killed", TickExitCause::Killed),
        ] {
            assert_eq!(exit_cause(None, None, Some(text)), Some(cause));
        }
        assert_eq!(
            exit_cause(Some("1"), None, Some("crashed")),
            Some(TickExitCause::ExitCode)
        );
        assert_eq!(
            exit_cause(None, Some("9"), None),
            Some(TickExitCause::Signal)
        );
    }

    #[test]
    fn retiring_gc_requires_absence_after_unloading_the_old_label() {
        let mut unloaded = false;
        assert!(stop_with(Path::new("/home"), "dev.provenant.gc", |args, _| {
            if args[0] == "bootout" {
                assert!(args[1].to_string_lossy().ends_with("/dev.provenant.gc"));
                unloaded = true;
                return Ok((true, String::new()));
            }
            Ok(if unloaded {
                (false, "exit 113: Could not find service \"dev.provenant.gc\" in domain for user gui: 501".into())
            } else {
                (true, "state = running".into())
            })
        }).unwrap());
        assert!(matches!(
            stop_with(Path::new("/home"), "dev.provenant.gc", |_, _| Ok((
                true,
                "state = running".into()
            ))),
            Err(AgentError::Quiescence {
                state: peer_agent::ServiceState::Running,
                ..
            })
        ));
    }

    #[test]
    fn timer_shutdown_requires_both_timer_and_dispatched_service_to_be_quiescent() {
        use peer_agent::ServiceState;
        for (output, expected) in [
            ((true, "LoadState=loaded\nActiveState=inactive"), None),
            (
                (true, "LoadState=loaded\nActiveState=active"),
                Some(ServiceState::Running),
            ),
            ((true, ""), Some(ServiceState::Indeterminate)),
            (
                (false, "timed out after 10s"),
                Some(ServiceState::Indeterminate),
            ),
        ] {
            let mut service_stopped = false;
            let result = quiesce_timer_with(|args| {
                if args[0] == "stop" && args[1] == "provenant-tick.service" {
                    service_stopped = true;
                }
                if args[0] == "show" {
                    assert!(service_stopped);
                    return if args[1] == "provenant-tick.service" {
                        Ok((output.0, output.1.into()))
                    } else {
                        Ok((true, "LoadState=loaded\nActiveState=inactive".into()))
                    };
                }
                Ok((true, String::new()))
            });
            match expected {
                None => assert!(result.unwrap()),
                Some(expected) => assert!(
                    matches!(result, Err(AgentError::Quiescence {state, ..}) if state == expected)
                ),
            }
        }
    }

    #[test]
    fn bootstrap_always_replaces_a_loaded_service_and_retries_after_bootout() {
        let (mut loaded, mut booted_out) = (true, false);
        bootstrap_with(Path::new("/home"), |args, _| match args.first().copied() {
            Some(command) if command == OsStr::new("print") => Ok((loaded, String::new())),
            Some(command) if command == OsStr::new("bootout") => {
                loaded = false;
                booted_out = true;
                Ok((true, String::new()))
            }
            Some(command) if command == OsStr::new("bootstrap") => {
                loaded = true;
                Ok((true, String::new()))
            }
            _ => Err(AgentError::Launchctl("unexpected", "command".into())),
        })
        .unwrap();
        assert!(booted_out && loaded);

        let mut first_bootstrap_failed = true;
        bootstrap_with_pause(
            Path::new("/home"),
            |args, _| {
                if args[0] == "bootstrap" {
                    let ok = !first_bootstrap_failed;
                    first_bootstrap_failed = false;
                    Ok((ok, "service is exiting".into()))
                } else {
                    Ok((true, String::new()))
                }
            },
            |_| {},
        )
        .unwrap();
        assert!(!first_bootstrap_failed);
    }

    fn xml_unescape(value: &str) -> String {
        value
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    }

    fn plist_value(xml: &str, key: &str) -> Option<String> {
        let marker = format!("<key>{key}</key>");
        let rest = xml.split_once(&marker)?.1;
        let (open, close, position) = [("<string>", "</string>"), ("<integer>", "</integer>")]
            .into_iter()
            .filter_map(|(open, close)| rest.find(open).map(|position| (open, close, position)))
            .min_by_key(|(_, _, position)| *position)?;
        let value = rest[position + open.len()..].split_once(close)?.0;
        Some(xml_unescape(value))
    }

    fn plist_array(xml: &str, key: &str) -> Vec<String> {
        let marker = format!("<key>{key}</key>");
        let Some(rest) = xml.split_once(&marker).map(|(_, rest)| rest) else {
            return Vec::new();
        };
        let Some(array) = rest
            .split_once("<array>")
            .and_then(|(_, array)| array.split_once("</array>"))
            .map(|(array, _)| array)
        else {
            return Vec::new();
        };
        let mut values = Vec::new();
        let mut rest = array;
        while let Some((_, value)) = rest.split_once("<string>") {
            let Some((value, tail)) = value.split_once("</string>") else {
                break;
            };
            values.push(xml_unescape(value));
            rest = tail;
        }
        values
    }

    fn plist_dictionary<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
        let marker = format!("<key>{key}</key>");
        xml.split_once(&marker)?
            .1
            .split_once("<dict>")?
            .1
            .split_once("</dict>")
            .map(|(dictionary, _)| dictionary)
    }

    fn plist_tag_after_key(xml: &str, key: &str, tag: &str) -> bool {
        let marker = format!("<key>{key}</key>");
        xml.split_once(&marker)
            .is_some_and(|(_, rest)| rest.trim_start().starts_with(tag))
    }

    fn plist_has_key(xml: &str, key: &str) -> bool {
        xml.contains(&format!("<key>{key}</key>"))
    }

    fn unit_values(text: &str, section: &str, key: &str) -> Vec<String> {
        let mut current = "";
        let mut values = Vec::new();
        for line in text.lines() {
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                current = name;
                continue;
            }
            if current == section
                && let Some((field, value)) = line.split_once('=')
                && field == key
            {
                values.push(value.to_owned());
            }
        }
        values
    }

    #[test]
    fn plist_renders_declared_fields_and_escapes_paths() {
        let program = Path::new("/bin/a&b");
        let state = Path::new("/state/a<b");
        let search = OsStr::new("/usr/bin:/opt/a&b");
        let text = String::from_utf8(render(program, state, search).unwrap()).unwrap();
        assert_eq!(plist_value(&text, "Label").as_deref(), Some(LABEL));
        let interval = INTERVAL.to_string();
        assert_eq!(
            plist_value(&text, "StartInterval").as_deref(),
            Some(interval.as_str())
        );
        assert_eq!(
            plist_array(&text, "ProgramArguments"),
            [program.to_string_lossy().into_owned(), "tick".into()]
        );
        let environment = plist_dictionary(&text, "EnvironmentVariables").unwrap();
        assert_eq!(
            plist_value(environment, "PATH").as_deref(),
            Some(search.to_str().unwrap())
        );
        assert_eq!(
            plist_value(environment, "PROVENANT_STATE_DIR").as_deref(),
            Some(state.to_str().unwrap())
        );
        for (key, path) in [
            ("StandardOutPath", state.join("tick/stdout.log")),
            ("StandardErrorPath", state.join("tick/stderr.log")),
        ] {
            assert_eq!(plist_value(&text, key).as_deref(), path.to_str());
        }
        assert!(plist_tag_after_key(&text, "RunAtLoad", "<true/>"));
        assert!(!plist_has_key(&text, "KeepAlive"));
    }

    #[test]
    fn timer_and_service_units_render_declared_fields_and_quote_arguments() {
        let unit = String::from_utf8(render_timer()).unwrap();
        assert_eq!(unit_values(&unit, "Timer", "OnActiveSec"), ["0"]);
        assert_eq!(
            unit_values(&unit, "Timer", "OnUnitActiveSec"),
            [format!("{INTERVAL}s")]
        );
        assert_eq!(unit_values(&unit, "Install", "WantedBy"), ["timers.target"]);

        let program = Path::new("/bin/a b\"%$");
        let state = Path::new("/state/x");
        let search = OsStr::new("/usr/bin:/opt/a");
        let text = String::from_utf8(render_service(program, state, search).unwrap()).unwrap();
        assert_eq!(unit_values(&text, "Service", "Type"), ["oneshot"]);
        assert_eq!(
            unit_values(&text, "Service", "ExecStart"),
            ["\"/bin/a b\\\"%%$$\" tick"]
        );
        assert_eq!(
            unit_values(&text, "Service", "Environment"),
            [
                "\"PATH=/usr/bin:/opt/a\"",
                "\"PROVENANT_STATE_DIR=/state/x\""
            ]
        );
        assert_eq!(unit_values(&text, "Service", "KillMode"), ["process"]);
        for (key, name) in [
            ("StandardOutput", "stdout.log"),
            ("StandardError", "stderr.log"),
        ] {
            assert_eq!(
                unit_values(&text, "Service", key),
                [format!(
                    "append:{}",
                    state.join(format!("tick/{name}")).display()
                )]
            );
        }
        let odd = String::from_utf8(
            render_service(
                Path::new("/b"),
                Path::new("/s p/100%"),
                OsStr::new("/a$b:/c"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            unit_values(&odd, "Service", "StandardOutput"),
            ["append:/s\\x20p/100%%/tick/stdout.log"]
        );
        assert_eq!(
            unit_values(&odd, "Service", "Environment"),
            ["\"PATH=/a$b:/c\"", "\"PROVENANT_STATE_DIR=/s\\x20p/100%%\""]
        );
        assert!(unit_values(&text, "Service", "Restart").is_empty());
        assert!(render_service(Path::new("/bin/a\nb"), Path::new("/s"), OsStr::new("/p")).is_err());
        assert_eq!(
            timer_in(Path::new("/h"), None),
            Path::new("/h/.config/systemd/user/provenant-tick.timer")
        );
        assert_eq!(
            timer_in(Path::new("/h"), None).with_file_name("provenant-tick.service"),
            Path::new("/h/.config/systemd/user/provenant-tick.service")
        );
    }

    #[test]
    fn timer_enable_is_idempotent_and_restarts_only_changed_units() {
        for (changed, expected) in [
            (false, TIMER_ENABLE_UNCHANGED),
            (true, TIMER_ENABLE_CHANGED),
        ] {
            let mut actual = Vec::new();
            let mut record = |args: &[&OsStr]| {
                actual.push(match args.first().copied() {
                    Some(command) if command == OsStr::new("daemon-reload") => {
                        assert_eq!(args.len(), 1);
                        EnableAction::Reload
                    }
                    Some(command) if command == OsStr::new("enable") => {
                        assert_eq!(args.get(1).copied(), Some(OsStr::new("--now")));
                        assert_eq!(args.get(2).copied(), Some(OsStr::new(TIMER)));
                        EnableAction::Enable
                    }
                    Some(command) if command == OsStr::new("restart") => {
                        assert_eq!(args.get(1).copied(), Some(OsStr::new(TIMER)));
                        EnableAction::Restart
                    }
                    _ => return Err(AgentError::Systemctl("unexpected", "command".into())),
                });
                Ok((true, String::new()))
            };
            enable_with(changed, &mut record).unwrap();
            assert_eq!(actual, expected);
        }
        let error = enable_with(false, |_| Ok((false, "no bus".into()))).unwrap_err();
        assert!(matches!(error, AgentError::Systemctl("enable", _)));
    }

    #[test]
    fn timer_disable_skips_an_absent_timer_and_propagates_failure() {
        let mut calls = Vec::new();
        disable_with(|args| {
            calls.push(args[0].to_owned());
            Ok((false, String::new()))
        })
        .unwrap();
        assert_eq!(calls, [OsStr::new("is-enabled"), OsStr::new("is-active")]);
        let mut calls = Vec::new();
        disable_with(|args| {
            calls.push(args[0].to_owned());
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(calls, [OsStr::new("is-enabled"), OsStr::new("disable")]);
        let error = disable_with(|args| Ok((args[0] != "disable", "busy".into()))).unwrap_err();
        assert!(matches!(error, AgentError::Systemctl("disable", _)));
    }
}
