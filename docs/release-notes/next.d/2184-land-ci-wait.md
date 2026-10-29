## Behaviour changes

- `land N` checks CI once. `land N --wait` waits up to one hour for the pinned head's required checks, then merges with a matching-head guard. Use `--wait=30m` to set a limit, up to four hours. Progress is bounded and stays off JSON stdout.
- Landing reports typed CI and head-change failures. A successful merge with refused local cleanup exits successfully with `cleanup_pending: true`; retrying an already-merged PR skips CI and merge and retries safe cleanup only.
- `pr status` reads the head and checks in one request per PR and retains successful rows when another PR is unavailable. `pr status N --wait` waits for a snapshot change instead of requiring a polling script.
- Landing re-reads the PR after any merge error and reports landed if it merged. It retries transient `gh` failures inside `--wait`, matches required checks on their app, treats a skipped required check as unavailable, and with no required check waits 60 seconds after the newest check registers. A queued or auto-merge result exits 5 as `pending`.
