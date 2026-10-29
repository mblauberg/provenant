## Fixes
### Telemetry and providers

- Native Claude subagent usage ingest now skips unrelated `.jsonl` filenames instead of aborting the report; a non-regular `agent-*.jsonl` file is still refused (#2284).
