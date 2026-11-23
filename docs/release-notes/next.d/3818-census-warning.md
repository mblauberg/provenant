## Fixes
### CLI and install

- Worktree and settlement commands no longer print an EPERM custody warning for
  other users' and system processes: only processes whose kernel credentials
  could own the checked-out worktree count towards it, and a same-user process
  that cannot be inspected still warns. An empty cohort prints nothing. (#3818)
