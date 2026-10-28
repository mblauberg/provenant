//! Per-user scheduled lane cache collection, published by the install receipt.
use provenant_config::GcSettings;
use provenant_fabric_runtime::peer_agent::{self, AgentError, launchctl, utf8, xml};
use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const LABEL: &str = "dev.provenant.gc";

pub fn plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/dev.provenant.gc.plist")
}

pub fn render(program: &Path, state: &Path, settings: &GcSettings) -> Result<Vec<u8>, AgentError> {
    let program = xml(utf8(program)?);
    let stdout = xml(utf8(&state.join("gc/stdout.log"))?);
    let stderr = xml(utf8(&state.join("gc/stderr.log"))?);
    let state = xml(utf8(state)?);
    let idle_days = settings.idle_days;
    Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>gc</string>
    <string>--scheduled</string>
    <string>--apply</string>
    <string>--idle-days</string>
    <string>{idle_days}</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>PROVENANT_STATE_DIR</key>
    <string>{state}</string>
  </dict>
  <key>StartInterval</key>
  <integer>86400</integer>
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

#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Installed {
    Started,
    Skipped,
    Failed { message: String },
}

pub fn install(state: &Path, home: &Path, changed: bool, enabled: bool) -> Installed {
    if !enabled || !peer_agent::eligible(state, home) {
        return Installed::Skipped;
    }
    let result = provenant_config::private_directory(&state.join("gc"), true)
        .map_err(|error| std::io::Error::other(error).into())
        .and_then(|_| bootstrap_with(home, changed, launchctl));
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

pub fn stop(home: &Path) -> Result<(), AgentError> {
    let service = format!("gui/{}/{LABEL}", provenant_os::fs::effective_uid());
    if launchctl(&["print".as_ref(), service.as_ref()], home)?.0 {
        let (ok, error) = launchctl(&["bootout".as_ref(), service.as_ref()], home)?;
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
        let settings = GcSettings {
            idle_days: 14,
            free_floor: Some("20G".into()),
            ..GcSettings::default()
        };
        let text = String::from_utf8(
            render(Path::new("/bin/a&b"), Path::new("/state/a<b"), &settings).unwrap(),
        )
        .unwrap();
        assert!(text.contains("<string>dev.provenant.gc</string>"));
        assert!(text.contains("<key>StartInterval</key>\n  <integer>86400</integer>"));
        assert!(text.contains("<string>/bin/a&amp;b</string>"));
        assert!(text.contains("<string>--scheduled</string>"));
        assert!(text.contains("<string>--apply</string>"));
        assert!(text.contains("<string>--idle-days</string>\n    <string>14</string>"));
        assert!(
            !text.contains("--free-floor"),
            "the collector must read the live shared floor"
        );
        assert!(text.contains("<key>PROVENANT_STATE_DIR</key>"));
        for path in ["stdout.log", "stderr.log"] {
            assert!(text.contains(&format!("<string>/state/a&lt;b/gc/{path}</string>")));
        }
        assert!(!text.contains("KeepAlive"));
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("new/state");
        let defaults = String::from_utf8(
            render(Path::new("/bin/provenant"), &state, &GcSettings::default()).unwrap(),
        )
        .unwrap();
        assert!(!defaults.contains("--free-floor"));
    }
}
