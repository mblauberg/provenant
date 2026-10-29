## Fixes

- Existing hard-linked files in a writable worktree stay readable but cannot be written, including those under ignored `target/` and `node_modules/` directories. On macOS the sandbox profile now covers all directories, and an overflow names the affected directory with recovery advice (#1951, #1957).
