### Fixed

- A worktree-write lane whose settlement could not publish its commits no longer leaves the bound worktree with the lane's files but the old HEAD and index. Settlement moves the lane's own files back, leaves any other edit untouched, and names the commit and branch holding the work in the settlement reason.
