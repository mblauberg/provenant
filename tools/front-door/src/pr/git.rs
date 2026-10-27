//! Git for the pull request helpers: bounded, with names checked first.
use super::{Result, execute_for};
use provenant_os::exec::Termination;
use std::{path::Path, time::Duration};

const GIT_TIMEOUT: Duration = Duration::from_secs(300);

/// A branch or remote name that cannot be read as an option or a revision
/// expression, so it is safe as a Git argument.
pub(super) fn plain_ref(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.contains("..")
        && !name
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\@{".contains(c))
}

pub(super) struct Git {
    executable: String,
}
impl Git {
    pub(super) fn new() -> Result<Self> {
        let git = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
        let executable = git
            .to_str()
            .ok_or("Git executable path is not UTF-8")?
            .to_owned();
        Ok(Self { executable })
    }
    /// Whether Git exited zero, and its first line of output or error.
    pub(super) fn run(&self, dir: &Path, args: &[&str]) -> Result<(bool, String)> {
        let mut argv = vec![self.executable.as_str()];
        argv.extend_from_slice(args);
        let output = execute_for(dir, &argv, GIT_TIMEOUT, false)?;
        let succeeded = output.termination == Termination::Exit(0);
        let bytes = if succeeded {
            &output.stdout.bytes
        } else {
            &output.stderr.bytes
        };
        let text = String::from_utf8_lossy(bytes);
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        Ok((succeeded, line.chars().take(200).collect()))
    }
    /// The remote `base` tracks, or `origin`.
    pub(super) fn remote_of(&self, dir: &Path, base: &str) -> Result<String> {
        let configured = self.full(dir, &["config", "--get", &format!("branch.{base}.remote")])?;
        Ok(configured
            .as_deref()
            .map(str::trim)
            .filter(|name| plain_ref(name) && *name != ".")
            .unwrap_or("origin")
            .to_owned())
    }
    pub(super) fn full(&self, dir: &Path, args: &[&str]) -> Result<Option<String>> {
        let mut argv = vec![self.executable.as_str()];
        argv.extend_from_slice(args);
        let output = execute_for(dir, &argv, GIT_TIMEOUT, false)?;
        Ok((output.termination == Termination::Exit(0))
            .then(|| String::from_utf8_lossy(&output.stdout.bytes).into_owned()))
    }
}
