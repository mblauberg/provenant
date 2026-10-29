## Breaking changes
### Pipelines and graphs

- **Graphs now use one host-configured limit set, and `--extend limits.*` no longer applies.** Raise the corresponding host limit before resuming a paused graph. Document limits and the deprecated `limit_ceiling` key are ignored with a warning (#2168).
