## Fixes

- `--model junior` now says "did you mean --route junior?" instead of only "model must be ADAPTER/ID" (#2350).
- A run-store contention refusal tells the caller to retry the same command with `--operation-id`, so a retry cannot admit twice (#2350).
