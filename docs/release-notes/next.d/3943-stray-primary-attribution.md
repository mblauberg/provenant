## Fixes

### Lanes and settlement

- A writer that committed on its bound branch no longer settles `review_required` because another process moved the primary checkout during its attempt. The host still observes the unaccounted move, now warns naming the new primary HEAD, and leaves it intact; the lane settles `succeeded` and the status text no longer claims the bound branch gained no commits. A lane whose bound branch stayed at base is still held as `stray_work` for review. Review lanes already read a pinned, read-only detached view of the PR head instead of checking it out in the primary checkout. (#3943)
