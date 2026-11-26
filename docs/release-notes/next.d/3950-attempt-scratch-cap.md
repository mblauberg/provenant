## Fixes

### Disk and garbage collection

- `provenant gc` now applies a per-attempt scratch size cap alongside the existing
  lane Cargo cache and attempt-directory payload caps. `gc.attempt_scratch_max`
  (default `2G`, set under `[gc]` or with `provenant config set`; `0` disables it)
  makes an over-cap scratch tree collectible once its attempt is settled, without
  waiting out `gc.idle`. The bounded observation is read-only: a tree it cannot
  measure is kept, never assumed over cap, and live, continuing or unsettled
  attempts stay protected. The effective cap and its source appear in GC output
  (`attempt scratch cap` in text, `attempt_scratch_max_bytes`/
  `attempt_scratch_max_source` in JSON). The stale claim that lane launches evict
  Cargo targets was removed from the knob registry. (#3950)
