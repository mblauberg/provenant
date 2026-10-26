//! Receipt-owned per-user disk pressure checks.
use provenant_fabric_runtime::peer_agent::{self, AgentError, launchctl, utf8, xml};
use serde::Serialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub const LABEL: &str = "dev.provenant.disk-guard";
pub fn plist(home: &Path) -> PathBuf {
    home.join("Library/LaunchAgents/dev.provenant.disk-guard.plist")
}
pub fn render(
    program: &Path,
    state: &Path,
    settings: &provenant_config::DiskGuardSettings,
) -> Result<Vec<u8>, AgentError> {
    let arguments = [
        ("--warn", &settings.warn),
        ("--pause", &settings.pause),
        ("--resume", &settings.resume),
    ]
    .into_iter()
    .map(|(flag, value)| {
        format!(
            "    <string>{flag}</string>\n    <string>{}</string>\n",
            xml(value)
        )
    })
    .chain(std::iter::once("    <string>--processes</string>\n".into()))
    .chain(
        settings
            .processes
            .iter()
            .map(|name| format!("    <string>{}</string>\n", xml(name))),
    )
    .collect::<String>();
    let program = xml(utf8(program)?);
    let stdout = xml(utf8(&state.join("disk-guard/stdout.log"))?);
    let stderr = xml(utf8(&state.join("disk-guard/stderr.log"))?);
    let state = xml(utf8(state)?);
    Ok(format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{program}</string>
    <string>disk-guard</string>
{arguments}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>/usr/bin:/bin:/usr/sbin:/sbin</string>
    <key>PROVENANT_STATE_DIR</key>
    <string>{state}</string>
  </dict>
  <key>ProcessType</key>
  <string>Background</string>
  <key>LowPriorityIO</key>
  <true/>
  <key>Nice</key>
  <integer>10</integer>
  <key>AbandonProcessGroup</key>
  <true/>
  <key>RunAtLoad</key>
  <true/>
  <key>StartInterval</key>
  <integer>120</integer>
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
impl Installed {
    pub fn describe(&self) -> String {
        match self {
            Self::Started => format!("{LABEL} checks every 2 minutes"),
            Self::Skipped => {
                "not installed (disabled or outside the macOS default state root)".into()
            }
            Self::Failed { message } => format!("failed: {message}"),
        }
    }
}
pub fn install(state: &Path, home: &Path, _changed: bool, enabled: bool) -> Installed {
    if !enabled || !peer_agent::eligible(state, home) {
        return Installed::Skipped;
    }
    match provenant_config::private_directory(&state.join("disk-guard"), true)
        .map_err(|error| AgentError::Io(std::io::Error::other(error)))
        .and_then(|_| {
            bootstrap_with(home, launchctl, || {
                crate::disk_guard::resume_saved(state).map_err(AgentError::Io)
            })
        }) {
        Ok(()) => Installed::Started,
        Err(error) => Installed::Failed {
            message: error.to_string(),
        },
    }
}
// launchd can briefly refuse bootstrap while the previous check exits.
#[allow(clippy::disallowed_methods)]
fn bootstrap_with(
    home: &Path,
    mut run: impl FnMut(&[&OsStr], &Path) -> Result<(bool, String), AgentError>,
    mut recover: impl FnMut() -> Result<(), AgentError>,
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
    // Reinstalling also recovers when settings are unchanged, including state
    // from versions that did not publish a recovery marker.
    // Restarting a paused checker must not strand the previous checker's PIDs.
    // Installation is a cold path and can read recovery state before bootstrap.
    recover()?;
    let path = plist(home);
    let mut last = String::new();
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
    crate::disk_guard::resume_saved(&home.join(".local/share/provenant/v1"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plist_runs_at_load_and_every_two_minutes_with_escaped_paths() {
        let text = String::from_utf8(
            render(
                Path::new("/bin/a&b"),
                Path::new("/state/a<b"),
                &provenant_config::DiskGuardSettings::default(),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(text.contains("<string>dev.provenant.disk-guard</string>"));
        assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
        assert!(text.contains("<key>StartInterval</key>\n  <integer>120</integer>"));
        assert!(text.contains("<string>/bin/a&amp;b</string>"));
        assert!(text.contains("<string>disk-guard</string>"));
        for file in ["stdout.log", "stderr.log"] {
            assert!(text.contains(&format!("<string>/state/a&lt;b/disk-guard/{file}</string>")));
        }
        assert!(text.contains("<key>PROVENANT_STATE_DIR</key>"));
        assert!(text.contains("<key>ProcessType</key>\n  <string>Background</string>"));
        assert!(text.contains("<key>LowPriorityIO</key>\n  <true/>"));
        assert!(text.contains("<key>Nice</key>\n  <integer>10</integer>"));
        // The scheduled background collector must survive the checker exiting.
        assert!(text.contains("<key>AbandonProcessGroup</key>\n  <true/>"));
        assert!(text.contains("<string>--warn</string>\n    <string>60G</string>"));
        assert!(text.contains("<string>--processes</string>"));
        assert!(text.contains("<string>clang++</string>"));
    }
    #[test]
    fn unchanged_loaded_agent_recovers_before_restarting() {
        let mut calls = Vec::new();
        let recovered = std::cell::Cell::new(false);
        bootstrap_with(
            Path::new("/home"),
            |args, _| {
                if args[0] == "bootstrap" {
                    assert!(recovered.get());
                }
                calls.push(args[0].to_owned());
                Ok((true, String::new()))
            },
            || {
                recovered.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            calls,
            [
                OsStr::new("print"),
                OsStr::new("bootout"),
                OsStr::new("bootstrap")
            ]
        );
        assert!(recovered.get());
    }
    #[test]
    fn restarting_agent_recovers_saved_processes_before_bootstrap() {
        let recovered = std::cell::Cell::new(false);
        bootstrap_with(
            Path::new("/home"),
            |args, _| {
                if args[0] == "bootstrap" {
                    assert!(recovered.get());
                }
                Ok((true, String::new()))
            },
            || {
                recovered.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(recovered.get());
    }
}
