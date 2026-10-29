## Fixes

- `telemetry stats` matches recorded model names to the bundled price table, estimates usage at API-equivalent prices and counts subscription-marked entries. Dated model snapshots are priced when the table lists them.
- A Claude Code sub-agent report groups native usage by model without message bodies and without needing a project or run store. Incomplete final transcript records are skipped and counted.
- Native JSON costs use money objects and name the reviewed price table.
