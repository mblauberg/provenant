## Additions
### Telemetry and providers

- `provenant telemetry ingest` records native Claude chair, sub-agent and lane usage per account from `~/.claude*` transcripts, keyed by session and message so a re-run adds nothing, and the chair's SessionStart hook runs it automatically within a three-second budget. Usage is attributed by config directory; unregistered directories are reported as `unattributed` rather than dropped, and no prompt or reply text is stored (#2239).
