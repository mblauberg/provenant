## Additions
### Telemetry and providers

- Chair sessions now show their tokens: `quota usage` and `telemetry stats` read each session's transcript and its sub-agents' for tokens, cache economics and API-equivalent cost beside lane spend. The SessionStart hook records the serving account, or `unattributed` for a config directory that is not a registered account, and sessions with an unreadable transcript are counted as unread, not as zero (#2173).
