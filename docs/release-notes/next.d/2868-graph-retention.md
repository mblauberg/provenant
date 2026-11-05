## Behaviour changes
### Pipelines and graphs

- Terminal graph checkpoints are retained for 90 days by default; `gc.graph_checkpoint_days` changes the retention period. Older telemetry reports can show a missing checkpoint (#2868).
