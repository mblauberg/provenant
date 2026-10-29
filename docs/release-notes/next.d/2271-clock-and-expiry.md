## Behaviour changes
### Accounts, quota and routing

- Routing schedules take RFC 3339 instants. `schedule.starts` and `schedule.expires` on policy rules and pacing overrides are quoted RFC 3339 timestamps in any offset, such as `expires = "2026-11-01T00:00:00Z"`, instead of Unix seconds. A rule or override that still has a number there is the wrong type, so it is dropped with a located warning. `provenant route override set` writes the canonical microsecond form (#2271).
- `provenant route list` JSON reports each account pacing expiry as `expires`, an RFC 3339 timestamp, instead of `expires_unix_seconds`. The text output and route pacing detail (`until=`) print the same timestamp (#2271).
- A quota calibration or experiment `expires` that is not an RFC 3339 timestamp is refused when the configuration is read, not later at validation or assignment (#2271).
- The settlement worker marker `settlement-queued.json` stores its expiry as an RFC 3339 `expires` (schema version 2) instead of `expires_ms`. Invalid markers and earlier formats fail explicitly during reconcile (#2271).
