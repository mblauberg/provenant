## Fixes
### Telemetry and providers

- `telemetry stats` now prices recorded models against the bundled price table, including dated snapshots, and counts subscription entries. The Claude Code sub-agent report groups native usage by model and skips incomplete transcript records, and native JSON costs name the reviewed price table (#1943).
