## Breaking changes
### Pipelines and graphs

- **Graph state and presets change.** Finish or cancel pending graphs and drain live lanes before upgrading every host. Port user presets by hand; retain old child-store directories for telemetry ingestion before garbage collection decides what to remove. See the [upgrade runbook](../../runbooks/install.md#bootstrap-and-upgrade). (#2867)

- **Retry a graph node and its dependants.** `provenant retry GRAPH --node NAME` redoes the selected node and downstream work, retaining prior attempts and recording the reset in the graph journal. Writer-child worktrees are reused during crash recovery.
