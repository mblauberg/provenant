//! Commit a shared child's declared paths after its custody is released.
use super::*;
use std::path::{Path, PathBuf};

/// Commit a finished shared child's claimed paths in the checkout it was
/// admitted to. `None` when the attempt did not run in a shared checkout.
pub(super) fn commit_shared_child(
    owner: &BoundGraphOwner<'_>,
    store: &provenant_run::RunStore,
    call: &provenant_run::Request<'_>,
    attempt: &provenant_records::run::AttemptRecord,
) -> Result<Option<String>, FacadeError> {
    let request = store.request(attempt.attempt_id, call).map_err(run_error)?;
    let Some(expected) = request.task.access.shared_worktree.as_ref() else {
        return Ok(None);
    };
    let corrupt = |detail: String| FacadeError::StateCorruptDetail { detail };
    let writer = request
        .task
        .writer
        .as_ref()
        .ok_or_else(|| corrupt("shared child request has no writer checkout".into()))?;
    let current = provenant_cli::fabric::inspect_checkout_subject(
        &owner.factory.context.roots,
        &writer.worktree,
    )
    .map_err(|error| {
        corrupt(format!(
            "inspect shared checkout {}: {error}",
            writer.worktree.display()
        ))
    })?;
    if current.as_ref() != Some(expected) {
        return Err(corrupt(format!(
            "shared checkout {} is no longer the one the child was admitted to",
            writer.worktree.display()
        )));
    }
    commit(
        &writer.worktree,
        &request.task.profile.write_scope,
        attempt.attempt_id,
        &owner.factory.home,
        &owner.factory.executor,
        owner.cancel,
    )
    .map(Some)
}

pub(super) fn commit(
    worktree: &Path,
    claims: &[String],
    attempt: provenant_core::AttemptId,
    home: &Path,
    executor: &provenant_os::exec::SystemExecutor,
    cancel: &provenant_core::CancelToken,
) -> Result<String, FacadeError> {
    let git = |args: &[&str]| -> Result<String, FacadeError> {
        let bytes = delivery_git_output(worktree, args)
            .map_err(|error| FacadeError::io(error.to_string()))?;
        String::from_utf8(bytes)
            .map(|s| s.trim().to_owned())
            .map_err(|error| FacadeError::io(error.to_string()))
    };
    let reference = git(&["symbolic-ref", "--quiet", "HEAD"])?;
    let marker = format!("Provenant-own-attempt: {attempt}");
    let grep = format!("--grep={marker}");
    let prior = git(&[
        "log",
        "-1",
        "--format=%H",
        "--fixed-strings",
        &grep,
        &reference,
    ])?;
    if !prior.is_empty() {
        return Ok(prior);
    }
    let head = git(&["rev-parse", "--verify", "HEAD"])?
        .parse::<provenant_core::GitOid>()
        .map_err(|error| FacadeError::io(error.to_string()))?;
    let git_dir =
        provenant_os::git::repository_path(worktree, provenant_os::git::RepositoryPath::GitDir)
            .map_err(|error| FacadeError::io(error.to_string()))?;
    let paths: Vec<PathBuf> = super::claims::effective_claims(claims)
        .map(PathBuf::from)
        .collect();
    let env = provenant_run::build::prepare::lane_environment(home);
    let result = provenant_run::commit_own_paths(provenant_run::OwnCommitRequest {
        worktree,
        git_dir: &git_dir,
        ref_name: &reference,
        expected: head,
        input: provenant_run::OwnCommitInput::Paths(&paths),
        message: &format!("lane: commit claimed paths\n\n{marker}"),
        retries: 1,
        env: &env,
        executor,
        cancel,
        pause: &provenant_core::ThreadPause,
        timeout: std::time::Duration::from_secs(30),
    })
    .map_err(|error| {
        run_error(provenant_run::RunError::Writer(
            provenant_confinement::worktrees::WorktreeError::CustodyCommand(Box::new(error)),
        ))
    })?;
    Ok(result.head.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn git(path: &Path, args: &[&str]) -> String {
        String::from_utf8(provenant_os::fixture::run_git(path, args).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }

    #[test]
    fn commits_only_claims_and_reconciles_repeated_observation() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        fs::write(repo.join("own"), "before").unwrap();
        fs::write(repo.join("sibling"), "before").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        fs::write(repo.join("own"), "after").unwrap();
        fs::write(repo.join("sibling"), "sibling edit").unwrap();
        let executor = provenant_os::exec::SystemExecutor::new(1).unwrap();
        let cancel = provenant_core::CancelToken::new();
        let id = provenant_core::AttemptId::from_bytes([42; 16]);
        let first = commit(&repo, &["own".into()], id, temp.path(), &executor, &cancel).unwrap();
        assert_eq!(git(&repo, &["show", "HEAD:own"]), "after");
        assert_eq!(git(&repo, &["show", "HEAD:sibling"]), "before");
        assert_eq!(
            fs::read_to_string(repo.join("sibling")).unwrap(),
            "sibling edit"
        );
        let again = commit(&repo, &["own".into()], id, temp.path(), &executor, &cancel).unwrap();
        assert_eq!(first, again);
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), first);
    }

    #[test]
    fn empty_claims_commit_the_whole_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        fs::write(repo.join("tracked"), "before").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        fs::write(repo.join("tracked"), "after").unwrap();
        fs::write(repo.join("new"), "new file").unwrap();
        let executor = provenant_os::exec::SystemExecutor::new(1).unwrap();
        let cancel = provenant_core::CancelToken::new();
        commit(
            &repo,
            &[],
            provenant_core::AttemptId::from_bytes([43; 16]),
            temp.path(),
            &executor,
            &cancel,
        )
        .unwrap();
        assert_eq!(git(&repo, &["show", "HEAD:tracked"]), "after");
        assert_eq!(git(&repo, &["show", "HEAD:new"]), "new file");
    }
}
