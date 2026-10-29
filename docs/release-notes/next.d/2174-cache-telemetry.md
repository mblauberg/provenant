## Additions

- Codex attempts record cache reads and writes: the decoder reads
  `cached_input_tokens` and `cache_write_input_tokens`, and subtracts both from
  uncached input. Cursor cache reads and writes are recorded too, and Claude's
  one-hour part of cache writes is kept as `cache_write_1h`.
- `telemetry stats` reports the cache-read share per model, account, provider
  and source (lane or chair), in text and under `cache` in JSON, with chair
  sessions in a `chair` section.
