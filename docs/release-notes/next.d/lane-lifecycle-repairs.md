## Fixes

- Resume explicit unowned and adopted worktrees in place, preserving edits.
  Launch-time ownership evidence keeps previously owned lane trees fenced
  after pruning, reassignment or owner removal.
- State that prior history is unavailable when resume needs a replacement
  worktree, avoiding unrelated primary-checkout commits in the fallback prompt.
