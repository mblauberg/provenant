## Fixes
### CLI and install

- `provenant gc` now protects the default branch on every retirement path. A
  remote ledger record naming the default branch is retained when no target was
  fetched, both in dry-run and when the remote default cannot be read; a linked
  worktree checked out on the default branch is never retired even when the
  primary checkout is elsewhere; and branches excluded by the retirement scope
  now report as Retained with a reason instead of vanishing from the preview.
  (#3768)
