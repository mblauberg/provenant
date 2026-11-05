## Fixes
### Lanes and settlement

- A short `provenant run --wait` no longer refuses a writer with `WRITER_LOCK_BUSY` while another worktree is being created. It now shortens the admission window to no less than 60 seconds. `--admission-timeout` still sets any shorter limit.
