## Fixes
### Lanes and settlement

- Hard-linked files in a writable worktree, including those under ignored `target/` and `node_modules/` directories, stay readable but cannot be written. On macOS the sandbox profile now covers all directories, and an overflow names the affected directory with recovery advice (#1951, #1957).
