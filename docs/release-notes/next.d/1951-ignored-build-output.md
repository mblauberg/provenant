## Fixes
### Lanes and settlement

- Hard-linked files in a writable worktree, including under ignored `target/` and `node_modules/`, stay readable but cannot be written. On macOS, a sandbox overflow now names the affected directory and how to recover (#1951, #1957).
