## Breaking changes
### Pipelines and graphs

- **Graph state and presets change.** Finish or cancel pending graphs and drain live lanes before upgrading every host. Port user presets by hand; retain old child-store directories for telemetry ingestion before garbage collection decides what to remove. See the [upgrade runbook](../../runbooks/install.md#bootstrap-and-upgrade). (#2867)
