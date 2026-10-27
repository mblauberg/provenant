## Fixes

- Read-only Claude lanes can read every `--read-root`, including roots outside
  their working directory. Each such root is now passed to Claude as an added
  directory, so a review briefed with a diff file outside the worktree no
  longer ends without a verdict after Claude denies the read (#1862).
