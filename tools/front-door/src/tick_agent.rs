//! Platform seam for the per-user host tick scheduler, published by the install receipt.
use provenant_fabric_runtime::peer_agent::{self, AgentError, launchctl, utf8, xml};
use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const LABEL: &str = "dev.provenant.tick";

pub fn plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/dev.provenant.tick.plist")
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
  <integer>60</integer>
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
    let result = provenant_config::private_directory(&state.join("tick"), true)
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
    stop_with(home, LABEL, launchctl)
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
}
