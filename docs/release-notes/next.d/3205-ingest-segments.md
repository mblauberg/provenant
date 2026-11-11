## Fixes
### Telemetry and providers

- Transcript ingest now keeps one segment per day, so `telemetry mileage` and chair and sub-agent spend keep working as history grows instead of failing at a fixed store size; an existing single-file store is split on first use. `telemetry stats --json` returns the summary the human view shows (per-attempt rows need `--detail`) and, when a result is still over 1 MiB, refuses it with `RESULT_TOO_LARGE` without leaving a file in `tmp/results`; a message streamed across UTC midnight is counted once, an old store's over-long lines keep the file as `ingest.jsonl.unmigrated` instead of being dropped, and stale ingest staging files are removed (#3205).
