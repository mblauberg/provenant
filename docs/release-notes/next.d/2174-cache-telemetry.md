## Additions
### Telemetry and providers

- Codex and Cursor attempts record cache reads and writes, and Claude's one-hour part of cache writes is kept as `cache_write_1h` (#2174).
- `telemetry stats` reports the cache-read share per model, account, provider and source (lane or chair), in text and under `cache` in JSON, with chair sessions in a `chair` section (#2174).
