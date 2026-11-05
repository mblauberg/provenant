//! Git for the pull request helpers: bounded, with names checked first.
use super::{Result, execute_for};
use provenant_os::exec::Termination;
use std::{path::Path, time::Duration};

pub(super) const GIT_TIMEOUT: Duration = Duration::from_secs(300);

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
    facts: provenant_confinement::worktrees::RepoFacts,
    utility: provenant_confinement::utility::UtilityRuntime,
    executor: provenant_os::exec::SystemExecutor,
    budget: provenant_confinement::utility::UtilityBudget,
    cancel: provenant_core::CancelToken,
    repositories: std::sync::Mutex<
        std::collections::BTreeMap<std::path::PathBuf, provenant_confinement::repo::ReadOnlyRepo>,
    >,
}
impl Git {
    pub(super) fn new() -> Result<Self> {
        let git = provenant_os::git::git_executable().ok_or("Git executable is unavailable")?;
        let executable = git
            .to_str()
            .ok_or("Git executable path is not UTF-8")?
            .to_owned();
        Ok(Self {
            executable,
            facts: provenant_confinement::worktrees::RepoFacts::default(),
            utility: provenant_confinement::utility::UtilityRuntime::read_only(git)?,
            executor: provenant_os::exec::SystemExecutor::new(1)?,
            budget: provenant_confinement::utility::UtilityBudget::bounded(GIT_TIMEOUT)?,
            cancel: provenant_core::CancelToken::new(),
            repositories: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        })
    }
    /// Resolve one ref using this command's bounded repository facts.
    pub(super) fn commit(
        &self,
        dir: &Path,
        reference: provenant_confinement::repo::GitRefName,
    ) -> Result<Option<provenant_core::GitOid>> {
        self.read_ref(dir, reference, true)
    }

    fn read_ref(
        &self,
        dir: &Path,
        reference: provenant_confinement::repo::GitRefName,
        peel: bool,
    ) -> Result<Option<provenant_core::GitOid>> {
        use provenant_confinement::worktrees::WorktreeError;
        let mut repositories = self
            .repositories
            .lock()
            .map_err(|_| "repository facts lock failed")?;
        let repo = match repositories.entry(dir.to_path_buf()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                self.utility
                    .open_read_repo(&self.executor, dir, self.budget, &self.cancel)?,
            ),
        };
        let result = if peel {
            self.facts.resolve_read_commit(
                repo,
                &self.executor,
                &self.utility,
                self.budget,
                reference,
                &self.cancel,
            )
        } else {
            self.facts.resolve_read_ref(
                repo,
                &self.executor,
                &self.utility,
                self.budget,
                reference,
                &self.cancel,
            )
        };
        match result {
            Ok(oid) => Ok(Some(oid)),
            Err(WorktreeError::NotFound) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Resolve a local branch through this command's facts session.
    pub(super) fn branch_commit(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<Option<provenant_core::GitOid>> {
        let Ok(reference) = provenant_confinement::repo::GitRefName::branch(branch) else {
            return Ok(None);
        };
        self.commit(dir, reference)
    }

    /// Check the unpeeled branch ref, including refs to non-commit objects.
    pub(super) fn branch_ref(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<Option<provenant_core::GitOid>> {
        let Ok(reference) = provenant_confinement::repo::GitRefName::branch(branch) else {
            return Ok(None);
        };
        self.read_ref(dir, reference, false)
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
    /// Git's exit code, whole stdout and first stderr line, for commands whose
    /// non-zero exit is an answer rather than a failure. A signal is an error.
    pub(super) fn exit(&self, dir: &Path, args: &[&str]) -> Result<(i64, String, String)> {
        let mut argv = vec![self.executable.as_str()];
        argv.extend_from_slice(args);
        let output = execute_for(dir, &argv, GIT_TIMEOUT, false)?;
        let Termination::Exit(code) = output.termination else {
            return Err("Git was terminated by a signal".into());
        };
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        let line = stderr
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        Ok((
            code,
            String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
            line.chars().take(200).collect(),
        ))
    }
}
