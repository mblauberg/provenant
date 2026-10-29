## Breaking changes
### Accounts, quota and routing
- **Pacing overrides moved to `provenant route override set|list|clear`.** `route pacing` now takes only `PROVIDER POLICY`; replace `route pacing override --account A --policy P --until 2h` with `route override set --account A --policy P --until 2h`, and `route pacing override clear` with `route override clear` (#2236).
- **Schedules are validated at load.** An unknown `tz`, `starts` at or after `expires`, equal or out-of-range hours, duplicate or out-of-range weekdays, or more than 16 windows make the record invalid; it no longer falls back to UTC (#2236).
