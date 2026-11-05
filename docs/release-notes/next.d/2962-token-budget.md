## Fixes
### Lanes and settlement

- A Claude writer lane that passes its `writer_token_budget` (default 500000, cached input included) is stopped and settles `timed_out` with the budget and its source in the reason; `--explain` shows both and names the providers it is not enforced for. Build-slot waits stay off the wall deadline for at most `lanes.slot_wait_seconds` in total (#2962).
