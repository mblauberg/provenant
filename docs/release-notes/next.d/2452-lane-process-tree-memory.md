## Fixes

- Confined macOS lanes can read their command tree's resident memory with
  `provenant process-tree --root PID --json`. The supervisor samples only the
  lane's own tree; unavailable or stale observations return exit 4.
