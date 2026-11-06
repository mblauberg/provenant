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

#[cfg(test)]
mod access_tests {
    use super::*;
    #[test]
    fn worktree_access_preserves_writer_and_implicit_reader_rules() {
        let path = Path::new("/checkout");
        assert!(worktree_access(true, false, Some(path)).is_ok());
        assert!(worktree_access(true, false, None).is_err());
        assert!(worktree_access(false, false, Some(path)).is_err());
        assert!(worktree_access(false, true, Some(path)).is_ok());
    }
}
