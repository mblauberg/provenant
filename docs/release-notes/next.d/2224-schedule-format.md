## Breaking changes
### Accounts, quota and routing
Routing policy rules and pacing overrides now persist their timing fields under `schedule`, with `starts`, `expires`, optional `tz` and `windows`. Existing `starts_unix_seconds`, `expires_unix_seconds` and top-level `windows` are converted at load; rewrite the configuration file to persist the new form. Expired legacy records are dropped.
