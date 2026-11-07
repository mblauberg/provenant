//! Operator-owned settings for one unattended pull-request service.
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    net::SocketAddr,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Observe,
    Act,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    pub repository: String,
    pub directory: PathBuf,
    pub checkout: PathBuf,
    pub executable: PathBuf,
    pub worker: PathBuf,
    /// Explicit PATH: launchd does not inherit an interactive shell's setup.
    pub path: String,
    #[serde(default = "sweep_seconds")]
    pub sweep_seconds: u64,
    #[serde(default = "poll_seconds")]
    pub poll_seconds: u64,
    #[serde(default)]
    pub mode: Mode,
    pub policy_digest: String,
    #[serde(default = "base_branch")]
    pub base_branch: String,
    #[serde(default)]
    pub bind: Option<SocketAddr>,
}

fn sweep_seconds() -> u64 {
    300
}
fn poll_seconds() -> u64 {
    5
}
fn base_branch() -> String {
    "main".into()
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Paths are absolute, UTF-8 and printable so generated settings and plists
/// refer to the same files without shell expansion or invalid XML characters.
pub fn absolute(path: &Path) -> io::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("service paths must be UTF-8"))?;
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || text.chars().any(char::is_control)
    {
        return Err(invalid(
            "service paths must be absolute without parent components or control characters",
        ));
    }
    Ok(text)
}

impl Settings {
    pub fn read(path: &Path) -> io::Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?.take(65_537).read_to_end(&mut bytes)?;
        if bytes.len() > 65_536 {
            return Err(invalid("service settings exceed 64 KiB"));
        }
        let settings: Self = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version != 1 {
            return Err(invalid("unsupported service settings version"));
        }
        let parts: Vec<_> = self.repository.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                !part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                    || !part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            })
        {
            return Err(invalid("service repository must be owner/name"));
        }
        for path in [
            &self.directory,
            &self.checkout,
            &self.executable,
            &self.worker,
        ] {
            absolute(path)?;
            if path.file_name().is_none() {
                return Err(invalid(
                    "service paths must name a file or directory below root",
                ));
            }
        }
        if self.bind.is_some_and(|address| !address.ip().is_loopback()) {
            return Err(invalid("service receiver must bind to loopback"));
        }
        if self.path.is_empty()
            || self
                .path
                .split(':')
                .any(|part| absolute(Path::new(part)).is_err())
        {
            return Err(invalid(
                "service PATH must contain only explicit absolute directories",
            ));
        }
        if !(1..=86_400).contains(&self.sweep_seconds)
            || !(1..=3_600).contains(&self.poll_seconds)
            || self.poll_seconds > self.sweep_seconds
        {
            return Err(invalid(
                "service intervals must be positive, with poll <= sweep <= 86400 and poll <= 3600",
            ));
        }
        if self.policy_digest.trim().is_empty()
            || self.policy_digest.len() > 256
            || self.policy_digest.chars().any(char::is_control)
        {
            return Err(invalid(
                "service policy digest must be a nonempty printable identifier",
            ));
        }
        let branch = &self.base_branch;
        if branch.is_empty()
            || branch.starts_with(['-', '/'])
            || branch.ends_with(['.', '/'])
            || branch.contains("..")
            || branch.contains("@{")
            || branch
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
            || branch
                .chars()
                .any(|c| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c))
        {
            return Err(invalid("invalid service base branch"));
        }
        if self.mode == Mode::Act {
            // Issue #2873 parks automatic action until the chair has live
            // observation and recovery evidence. No file or boolean can promote
            // this building block into an automatic review/repair/landing agent.
            return Err(invalid(
                "automatic actions remain parked pending observe-only measurements and restart evidence",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub fn settings() -> Settings {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "repository": "example/project",
            "directory": "/tmp/service",
            "checkout": "/tmp/project",
            "executable": "/opt/provenant/bin/provenant",
            "worker": "/opt/provenant/bin/pr-reconciler",
            "path": "/opt/provenant/bin:/usr/bin:/bin",
            "policy_digest": "policy-1"
        }))
        .unwrap()
    }

    #[test]
    fn settings_default_to_observing_and_refuse_implicit_authority() {
        let mut found = settings();
        found.validate().unwrap();
        assert_eq!(found.mode, Mode::Observe);
        assert_eq!(found.sweep_seconds, 300);
        assert_eq!(found.poll_seconds, 5);
        assert_eq!(found.base_branch, "main");
        found.mode = Mode::Act;
        assert!(found.validate().is_err());
        let mut json = serde_json::to_value(&found).unwrap();
        json["authority"] = serde_json::json!({"land": true});
        assert!(serde_json::from_value::<Settings>(json).is_err());
    }

    #[test]
    fn settings_refuse_ambiguous_paths_repositories_and_schedules() {
        for repository in [
            "",
            "../repo",
            "owner/../repo",
            "owner/repo?x",
            "owner/repo\n",
        ] {
            let mut found = settings();
            found.repository = repository.into();
            assert!(found.validate().is_err(), "{repository:?}");
        }
        for path in [
            "",
            ":/bin",
            "/bin:",
            "/bin:relative",
            "/bin:/tmp/../bin",
            "/bin\n",
        ] {
            let mut found = settings();
            found.path = path.into();
            assert!(found.validate().is_err(), "{path:?}");
        }
        for path in ["relative", "/tmp/../service", "/tmp/\u{1}", "/"] {
            let mut found = settings();
            found.directory = path.into();
            assert!(found.validate().is_err(), "{path:?}");
        }
        for (poll, sweep) in [(0, 300), (5, 0), (301, 300), (1, 86_401), (3_601, 4_000)] {
            let mut found = settings();
            found.poll_seconds = poll;
            found.sweep_seconds = sweep;
            assert!(found.validate().is_err());
        }
        let mut found = settings();
        found.bind = Some("0.0.0.0:8765".parse().unwrap());
        assert!(found.validate().is_err());
        found.bind = Some("127.0.0.1:8765".parse().unwrap());
        found.validate().unwrap();
        for branch in [
            "-main",
            "../main",
            "heads//main",
            "main.lock",
            "main@{x}",
            "main\n",
            "main?",
        ] {
            let mut found = settings();
            found.base_branch = branch.into();
            assert!(found.validate().is_err(), "{branch:?}");
        }
    }
}
