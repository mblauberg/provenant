use super::*;

/// Worktree access is explicit. Submission resolves and verifies the path;
/// only an explicit read-only mode may bind without writer authority.
pub fn worktree_access(
    writer: bool,
    explicit_reader: bool,
    worktree: Option<&Path>,
) -> Result<(), Fail> {
    match worktree {
        None if !writer => Ok(()),
        None => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeRequired,
            "worktree-write access needs --worktree, or --branch and --base with a peer --host",
        )),
        Some(_) if !writer && !explicit_reader => Err(Fail::new(
            Status::Invalid,
            ErrorKind::WorktreeNotApplicable,
            "--worktree requires --mode worktree-write",
        )),
        Some(_) => Ok(()),
    }
}

