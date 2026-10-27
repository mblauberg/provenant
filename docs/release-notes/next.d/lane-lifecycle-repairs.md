## Fixes

- Resume explicit unowned and adopted worktrees in place, preserving edits.
  Launch-time ownership evidence keeps previously owned lane trees fenced
  after pruning, reassignment or owner removal.
- State that prior history is unavailable when resume needs a replacement
  worktree, avoiding unrelated primary-checkout commits in the fallback prompt.
- Refuse protected direct-push targets before admission for concise delivery,
  legacy flags and manifests. PRs may still target protected base branches.
- Recreate deleted graph publication refs on retry when the sealed marker
  matches the same head. Conflicting markers and refs still refuse.
- Preserve accepted dispatch replay when later repository state protects the
  publication branch; new admission and actual delivery still check protection.
