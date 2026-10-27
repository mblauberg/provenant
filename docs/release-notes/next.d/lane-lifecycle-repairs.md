## Fixes

- Resume explicit unowned and adopted worktrees in place, preserving edits.
  Launch-time ownership evidence keeps previously owned lane trees fenced
  after pruning, reassignment or owner removal. Attempts recorded before
  #1855 have no launch evidence of running under an owned tree, so continuation
  resumes them in place (fail open).
- State that prior history is unavailable when resume needs a replacement
  worktree, avoiding unrelated primary-checkout commits in the fallback prompt.
- Refuse protected direct-push targets before admission for concise delivery,
  legacy flags and manifests. PRs may still target protected base branches.
- Recreate deleted graph publication refs on retry when the sealed marker
  matches the same head. Conflicting markers and refs still refuse.
- Preserve accepted dispatch replay when later repository state protects the
  publication branch; new admission and actual delivery still check protection.
