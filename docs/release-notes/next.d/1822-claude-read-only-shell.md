## Fixes

- Read-only Claude lanes now have Bash, so a review lane can run `git log -p`,
  `git diff` and `grep`. The shell gets the same controls as a writer's:
  - a scrubbed subprocess environment;
  - isolated git config;
  - no keychain access;
  - only the system binaries, with no cargo or node tools.

  The read-only profile still denies writes to the worktree and other project
  paths. Like writer shells, the shell can still write the Fabric store and its
  own session transcript, and it shares the lane's network access.
