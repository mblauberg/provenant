## Fixes
### Lanes and settlement

- A Claude or Codex writer lane that passes its `writer_token_budget` (default 500000, fresh tokens only; cache reads excluded) is stopped and settles `timed_out` with its uncommitted work committed on its branch, and the budget and its source in the reason; `--explain` shows both and names the providers it is not enforced for. Build-slot waits stay off the wall deadline for at most `lanes.slot_wait_seconds` in total (#2962).
