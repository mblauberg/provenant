## Breaking changes
### Accounts, quota and routing
Routing policy rules and pacing overrides now keep their timing fields under `schedule`, with `starts`, `expires`, optional `tz` and `windows`. The configuration is refused while a record still carries `starts_unix_seconds`, `expires_unix_seconds` or a top-level `windows`; the error names the record and the key's replacement (`moved_to_schedule.starts`, `.expires` or `.windows`). Move each value under `schedule`.
