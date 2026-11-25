## Fixes

### Lanes and settlement

- A live provider that stops emitting output is now visible instead of reading as `running` forever. `providers.<id>.stall_after_seconds` (compiled default 1800 seconds, `0` disables) measures only output-sourced activity, never the queued or started fallback; the threshold and its layer appear in `run --explain` and on the lane's recorded limits. `status --json` carries `stalled: true` with the last-event time, the chair's digest prints `stalled last_event=<time>`, and `wait` ends with the `stalled` outcome (exit code 6) without claiming settlement. Detection only warns (`PROVIDER_STALLED`), so a slow but healthy provider is never stopped on a guess. `providers.<id>.stall_policy` is opt-in: `report` (default) warns only; `steer` queues one steering turn; `retry` and `fail` stop the provider from run-side custody after the warning and settle with the typed `provider_stalled` cause. (#3903)
