## Additions
### Accounts, quota and routing
Routing policy and pacing overrides share schedules with optional weekdays and IANA time zones. `provenant route override list` shows recorded schedules, active state and time remaining. Policy fallback models and efforts that no longer match the catalogue are warned about and dropped during evaluation.

## Breaking changes
Routing policy rules and pacing overrides now persist their timing fields under `schedule`, with `starts`, `expires`, optional `tz` and `windows`. Existing `starts_unix_seconds`, `expires_unix_seconds` and top-level `windows` are converted at load; rewrite the configuration file to persist the new form. Expired legacy records are dropped.
