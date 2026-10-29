## Behaviour changes

- `land N` checks CI once. `land N --wait` waits up to one hour for the pinned head's required checks, then merges with a matching-head guard. Use `--wait=30m` to set a limit, up to four hours. Progress is bounded and stays off JSON stdout.
- Landing reports typed CI and head-change failures. A successful merge with refused local cleanup exits successfully with `cleanup_pending: true`; retrying an already-merged PR skips CI and merge and retries safe cleanup only.
- `pr status` reads the head and checks in one request per PR and retains successful rows when another PR is unavailable. `pr status N --wait` waits for a snapshot change instead of requiring a polling script.
