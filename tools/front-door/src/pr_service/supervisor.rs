//! Offline launchd generation. Applying the returned plist is operator-owned.
use super::settings::{Settings, absolute};
use provenant_fabric_runtime::peer_agent::xml;
use sha2::{Digest, Sha256};
use std::{io, path::Path};

/// One label per repository, independent of punctuation in its owner/name.
pub fn label(settings: &Settings) -> String {
    let digest = Sha256::digest(settings.repository.as_bytes());
    let digest: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("dev.provenant.pr.{digest}")
}

pub fn render(settings: &Settings, config_path: &Path) -> io::Result<String> {
    settings.validate()?;
    let config = xml(absolute(config_path)?);
    let worker = xml(absolute(&settings.worker)?);
    let checkout = xml(absolute(&settings.checkout)?);
    let path = xml(&settings.path);
    let stdout = xml(absolute(&settings.directory.join("worker.stdout.log"))?);
    let stderr = xml(absolute(&settings.directory.join("worker.stderr.log"))?);
    let label = label(settings);
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array><string>{worker}</string><string>--config</string><string>{config}</string></array>
  <key>WorkingDirectory</key><string>{checkout}</string>
  <key>EnvironmentVariables</key><dict><key>PATH</key><string>{path}</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>30</integer>
  <key>Umask</key><integer>63</integer>
  <key>StandardOutPath</key><string>{stdout}</string>
  <key>StandardErrorPath</key><string>{stderr}</string>
</dict>
</plist>
"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_supervisor_restarts_with_throttling_and_explicit_arguments() {
        let mut settings = super::super::settings::tests::settings();
        settings.worker = "/opt/example & project/pr-reconciler".into();
        let found = render(&settings, Path::new("/tmp/config <1>.json")).unwrap();
        assert!(found.contains("<key>RunAtLoad</key><true/>"));
        assert!(found.contains("<key>KeepAlive</key><true/>"));
        assert!(found.contains("<key>ThrottleInterval</key><integer>30</integer>"));
        assert!(found.contains("<key>Umask</key><integer>63</integer>"));
        assert!(found.contains("<array><string>/opt/example &amp; project/pr-reconciler</string><string>--config</string><string>/tmp/config &lt;1&gt;.json</string></array>"));
        assert!(found.contains("<key>WorkingDirectory</key><string>/tmp/project</string>"));
        assert!(found.contains("<key>PATH</key><string>/opt/provenant/bin:/usr/bin:/bin</string>"));
        assert!(found.contains("<string>/tmp/service/worker.stdout.log</string>"));
        assert!(found.contains("<string>/tmp/service/worker.stderr.log</string>"));
        assert!(render(&settings, Path::new("relative.json")).is_err());
        let first = label(&settings);
        settings.repository = "example/other-project".into();
        assert_ne!(first, label(&settings));
    }
}
