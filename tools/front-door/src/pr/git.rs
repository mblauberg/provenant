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
    /// The full ID of the commit `name` identifies: a full ID as given, or any
    /// object name Git resolves to exactly one commit. Git names an ambiguous
    /// abbreviation, which is refused rather than guessed.
    pub(super) fn resolve_commit(&self, dir: &Path, name: &str) -> Result<provenant_core::GitOid> {
        if let Ok(oid) = provenant_core::GitOid::parse(name) {
            return Ok(oid);
        }
        let spec = format!("{name}^{{commit}}");
        let (code, stdout, error) =
            self.exit(dir, &["rev-parse", "--verify", "--end-of-options", &spec])?;
        let resolved = stdout.trim();
        if code != 0 || resolved.is_empty() {
            let reason = if error.is_empty() {
                "no such commit in this repository"
            } else {
                error.as_str()
            };
            return Err(super::wait::failure(
                "HEAD_UNRESOLVED",
                format!(
                    "cannot resolve --head {name} to one commit ({reason}); next: fetch it, or pass a longer prefix or the full ID"
                ),
            ));
        }
        Ok(provenant_core::GitOid::parse(resolved)?)
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
        self.with_repo(dir, |repo| {
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
        })
    }

    fn with_repo<T>(
        &self,
        dir: &Path,
        action: impl FnOnce(&provenant_confinement::repo::ReadOnlyRepo) -> Result<T>,
    ) -> Result<T> {
        let mut repositories = self
            .repositories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let repo = match repositories.entry(dir.to_path_buf()) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => entry.insert(
                self.utility
                    .open_read_repo(&self.executor, dir, self.budget, &self.cancel)?,
            ),
        };
        action(repo)
    }

    /// Return paths from a revalidated checkout, without another Git discovery.
    pub(super) fn repository_paths(
        &self,
        dir: &Path,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
        self.with_repo(dir, |repo| {
            repo.revalidate()?;
            Ok((
                repo.root_path().to_path_buf(),
                repo.private_git_dir().to_path_buf(),
                repo.common_git_dir().to_path_buf(),
            ))
        })
    }

    /// A command-controlled mutation can change repository configuration or
    /// remove a checkout, so later reads must bind and probe it again.
    pub(super) fn invalidate_repositories(&self) {
        self.repositories
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }

    /// Resolve the tree of a commit using the same bounded facts session.
    pub(super) fn commit_tree(
        &self,
        dir: &Path,
        commit: provenant_core::GitOid,
    ) -> Result<provenant_core::GitOid> {
        self.with_repo(dir, |repo| {
            Ok(self.facts.resolve_read_tree(
                repo,
                &self.executor,
                &self.utility,
                self.budget,
                commit,
                &self.cancel,
            )?)
        })
    }

    /// Read worktree registrations through this command's facts session.
    pub(super) fn worktrees(&self, dir: &Path) -> Result<Vec<u8>> {
        self.with_repo(dir, |repo| {
            Ok(self.facts.read_only_inventory(
                repo,
                &self.executor,
                &self.utility,
                self.budget,
                &self.cancel,
            )?)
        })
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

#[cfg(test)]
#[allow(unused_imports, dead_code)] // The custom worktree driver compiles without a test harness.
mod resolve_commit_tests {
    use super::*;

    /// A repository of many commits on one branch, so two of them are certain
    /// to share a four-character abbreviation.
    fn crowded_repository(commits: usize) -> (tempfile::TempDir, Vec<String>) {
        let temp = tempfile::tempdir().unwrap();
        let git = provenant_os::git::git_executable().unwrap();
        let git = git.to_str().unwrap();
        let run = |args: &[&str], input: &[u8]| {
            let mut argv = vec![git];
            argv.extend_from_slice(args);
            let output =
                super::super::execute_with(temp.path(), &argv, GIT_TIMEOUT, false, Some(input))
                    .unwrap();
            assert_eq!(output.termination, Termination::Exit(0), "{args:?}");
            String::from_utf8(output.stdout.bytes).unwrap()
        };
        run(&["init", "-q"], b"");
        let mut stream = String::new();
        for index in 0..commits {
            let message = format!("commit {index}\n");
            stream.push_str(&format!(
                "commit refs/heads/main\ncommitter T <t@example.invalid> {index} +0000\ndata {}\n{message}\n",
                message.len()
            ));
        }
        run(&["fast-import", "--quiet"], stream.as_bytes());
        let listed = run(&["rev-list", "main"], b"");
        (temp, listed.lines().map(str::to_owned).collect())
    }

    #[test]
    fn head_resolves_an_unambiguous_name_and_refuses_an_ambiguous_one() {
        let (temp, commits) = crowded_repository(3000);
        let git = Git::new().unwrap();
        let tip = commits[0].clone();
        let resolved = git.resolve_commit(temp.path(), &tip[..20]).unwrap();
        assert_eq!(resolved.to_string(), tip);
        assert_eq!(
            git.resolve_commit(temp.path(), "main").unwrap().to_string(),
            tip
        );
        // A full ID is taken as given, even when this repository lacks it.
        let absent = "1".repeat(40);
        assert_eq!(
            git.resolve_commit(temp.path(), &absent)
                .unwrap()
                .to_string(),
            absent
        );

        let mut seen = std::collections::BTreeSet::new();
        let shared = commits
            .iter()
            .find_map(|id| (!seen.insert(id[..4].to_owned())).then(|| id[..4].to_owned()))
            .expect("3000 commits share a four-character prefix");
        let error = git
            .resolve_commit(temp.path(), &shared)
            .expect_err("an ambiguous prefix must be refused")
            .to_string();
        assert!(error.contains("ambiguous"), "{error}");
        assert!(error.contains("full ID"), "{error}");
        assert!(git.resolve_commit(temp.path(), "feedfacefeed").is_err());
    }
}
