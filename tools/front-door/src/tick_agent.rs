//! Platform seam for the per-user host tick scheduler, published by the install receipt.
use provenant_fabric_runtime::peer_agent::{self, AgentError, launchctl, systemctl, utf8, xml};
use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const LABEL: &str = "dev.provenant.tick";
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
    provenant_config::systemd_user_dir(home).join(TIMER)
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
                bootstrap_with(home, changed, launchctl)
            }
        });
    match result {
        Ok(()) => Installed::Started,
        Err(error) => Installed::Failed {
            message: format!("{error}"),
        },
    }
}

// LaunchAgent bootstrap retries are bounded real-time OS operations.
#[allow(clippy::disallowed_methods)]
fn bootstrap_with(
    home: &Path,
    changed: bool,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let domain = format!("gui/{}", provenant_os::fs::effective_uid());
    let service = format!("{domain}/{LABEL}");
    let loaded = run(&["print".as_ref(), service.as_ref()], home)?.0;
    if loaded && !changed {
        return Ok(());
    }
    if loaded {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    let path = plist(home);
    let mut last = String::new();
    // launchd can refuse bootstrap briefly while a booted-out service exits.
    for _ in 0..5 {
        let (ok, error) = run(
            &["bootstrap".as_ref(), domain.as_ref(), path.as_os_str()],
            home,
        )?;
        if ok {
            return Ok(());
        }
        last = error;
        std::thread::park_timeout(std::time::Duration::from_millis(300));
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
    if changed {
        step("daemon-reload", &["daemon-reload"])?;
    }
    step("enable", &["enable", "--now", TIMER])?;
    if changed {
        step("restart", &["restart", TIMER])?;
    }
    Ok(())
}

/// Disable and stop the timer when it is enabled or running; absent is already stopped.
fn disable_with(
    mut run: impl FnMut(&[&OsStr]) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let present = run(&["is-enabled".as_ref(), TIMER.as_ref()])?.0
        || run(&["is-active".as_ref(), TIMER.as_ref()])?.0;
    if present {
        let (ok, error) = run(&["disable".as_ref(), "--now".as_ref(), TIMER.as_ref()])?;
        if !ok {
            return Err(AgentError::Systemctl("disable", error));
        }
    }
    Ok(())
}

pub fn stop(home: &Path) -> Result<(), AgentError> {
    if cfg!(target_os = "linux") {
        disable_with(|args| systemctl(args, home))
    } else {
        stop_with(home, LABEL, launchctl)
    }
}

/// Retire the former scheduler even when its plist has already disappeared.
pub fn retire_gc(home: &Path) -> Result<(), AgentError> {
    stop_with(home, "dev.provenant.gc", launchctl)
}

fn stop_with(
    home: &Path,
    label: &str,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
) -> Result<(), AgentError> {
    let service = format!("gui/{}/{label}", provenant_os::fs::effective_uid());
    if run(&["print".as_ref(), service.as_ref()], home)?.0 {
        let (ok, error) = run(&["bootout".as_ref(), service.as_ref()], home)?;
        if !ok {
            return Err(AgentError::Launchctl("bootout", error));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn retiring_gc_unloads_the_old_label_and_propagates_bootout_failure() {
        let mut calls = Vec::new();
        stop_with(Path::new("/home"), "dev.provenant.gc", |args, _| {
            calls.push((args[0].to_owned(), args[1].to_owned()));
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "print");
        assert_eq!(calls[1].0, "bootout");
        assert!(calls[1].1.to_string_lossy().ends_with("/dev.provenant.gc"));
        let error = stop_with(Path::new("/home"), "dev.provenant.gc", |args, _| {
            Ok((args[0] == "print", "busy".into()))
        })
        .unwrap_err();
        assert!(matches!(error, AgentError::Launchctl("bootout", _)));
    }

    #[test]
    fn bootstrap_keeps_an_unchanged_service_and_retries_after_bootout() {
        let mut calls = Vec::new();
        bootstrap_with(Path::new("/home"), false, |args, _| {
            calls.push(args[0].to_owned());
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(calls, [OsStr::new("print")]);
        let mut bootstraps = 0;
        bootstrap_with(Path::new("/home"), true, |args, _| {
            if args[0] == "bootstrap" {
                bootstraps += 1;
                Ok((bootstraps > 1, "service is exiting".into()))
            } else {
                Ok((true, String::new()))
            }
        })
        .unwrap();
        assert_eq!(bootstraps, 2);
    }

    #[test]
    fn plist_renders_interval_arguments_state_logs_and_xml_escaping() {
        let text = String::from_utf8(
            render(
                Path::new("/bin/a&b"),
                Path::new("/state/a<b"),
                OsStr::new("/usr/bin:/opt/a&b"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(text.contains("<string>dev.provenant.tick</string>"));
        assert!(text.contains("<key>StartInterval</key>\n  <integer>60</integer>"));
        assert!(text.contains("<string>/bin/a&amp;b</string>"));
        assert!(text.contains("<string>tick</string>"));
        assert!(text.contains("<key>PROVENANT_STATE_DIR</key>"));
        assert!(text.contains("<string>/usr/bin:/opt/a&amp;b</string>"));
        for path in ["stdout.log", "stderr.log"] {
            assert!(text.contains(&format!("<string>/state/a&lt;b/tick/{path}</string>")));
        }
        assert!(!text.contains("KeepAlive"));
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
    }

    #[test]
    fn timer_and_service_units_render_interval_command_state_logs_and_quoting() {
        let unit = String::from_utf8(render_timer()).unwrap();
        assert!(unit.contains("OnActiveSec=0\nOnUnitActiveSec=60s\n"));
        assert!(unit.contains("WantedBy=timers.target"));
        let text = String::from_utf8(
            render_service(
                Path::new("/bin/a b\"%$"),
                Path::new("/state/x"),
                OsStr::new("/usr/bin:/opt/a"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(text.contains("Type=oneshot\n"));
        assert!(text.contains("ExecStart=\"/bin/a b\\\"%%$$\" tick\n"));
        assert!(text.contains("Environment=\"PATH=/usr/bin:/opt/a\"\n"));
        assert!(text.contains("Environment=\"PROVENANT_STATE_DIR=/state/x\"\n"));
        assert!(text.contains("KillMode=process\n"));
        assert!(text.contains("StandardOutput=append:/state/x/tick/stdout.log\n"));
        assert!(text.contains("StandardError=append:/state/x/tick/stderr.log\n"));
        let odd = String::from_utf8(
            render_service(
                Path::new("/b"),
                Path::new("/s p/100%"),
                OsStr::new("/a$b:/c"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(odd.contains("StandardOutput=append:/s\\x20p/100%%/tick/stdout.log\n"));
        assert!(odd.contains("Environment=\"PATH=/a$b:/c\"\n"));
        assert!(!text.contains("Restart="));
        assert!(render_service(Path::new("/bin/a\nb"), Path::new("/s"), OsStr::new("/p")).is_err());
        assert_eq!(
            timer(Path::new("/h")),
            Path::new("/h/.config/systemd/user/provenant-tick.timer")
        );
        assert_eq!(
            service(Path::new("/h")),
            Path::new("/h/.config/systemd/user/provenant-tick.service")
        );
    }

    #[test]
    fn timer_enable_is_idempotent_and_restarts_only_changed_units() {
        let mut calls: Vec<String> = Vec::new();
        let mut record = |args: &[&OsStr]| {
            calls.push(
                args.iter()
                    .map(|a| a.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
            Ok((true, String::new()))
        };
        enable_with(false, &mut record).unwrap();
        enable_with(true, &mut record).unwrap();
        assert_eq!(
            calls,
            [
                "enable --now provenant-tick.timer",
                "daemon-reload",
                "enable --now provenant-tick.timer",
                "restart provenant-tick.timer",
            ]
        );
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
