## Breaking changes
### Pipelines and graphs

- **One policy decides what follows a stopped lane.** Graphs can continue, select another eligible route or return control to the chair according to the [stop table](../../specs/run.md#what-follows-a-stop); node policy can override unpinned causes. Standalone runs no longer continue themselves, so replace old continuation settings with graph stop policy. (#3171)
