## Additions
### Pipelines and graphs

`provenant tick` advances graphs with expired persisted CI or decision wake
deadlines under graph journal custody. CI polling has configurable backoff and
node overrides. macOS install and upgrade replace the separate GC LaunchAgent
with one 60-second `dev.provenant.tick` agent; GC becomes a daily tick task.
