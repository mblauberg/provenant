## Fixes
### Telemetry and providers

- `telemetry stats` matches recorded model names to the bundled price table, estimates usage at API-equivalent prices and counts subscription-marked entries. Dated model snapshots are priced when the table lists them (#1943).
- The Claude Code sub-agent report groups native usage by model, without message bodies and without a project or run store. Incomplete final transcript records are skipped and counted (#1943).
- Native JSON costs use money objects and name the reviewed price table (#1943).
