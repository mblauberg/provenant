## Fixes

- Existing hard-linked files in a writable worktree stay readable but are write-denied, including in ignored `target/` and `node_modules/` directories. The macOS census includes all directories, with bounded path counts and profile bytes; overflow names the affected directory and gives recovery advice (#1951, #1957).
