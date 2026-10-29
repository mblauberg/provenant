## Additions
- Add layered compaction, cache and lane policy per adapter, model and account, with UTC schedules and expiring overrides. Claude launches receive the configured compaction window; read-heavy and review routing can prefer cheap caches with a visible reason.
- Bundle task-boundary compaction guidance, DeepSeek Flash cache economics and bounded Terra continuation guidance. Report advisory cache-read share targets against the policy in `telemetry stats`, under `cache_read_share` in JSON.
