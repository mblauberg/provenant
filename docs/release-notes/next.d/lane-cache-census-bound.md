## Fixes

- A `worktree-write` dispatch is no longer refused with "compiled profile
  exceeds its byte bound" once a worktree's lane build cache passes 100,000
  entries. The cache census now allows 500,000 entries and keeps the
  hard-link check. Entry, depth and grant-count overflows now name their
  family and path instead of reporting the profile byte bound.
