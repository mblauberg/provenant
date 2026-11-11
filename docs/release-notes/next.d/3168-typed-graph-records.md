## Breaking changes
### Pipelines and graphs

- **Graph checkpoints store the pause, the writer handoff and review decisions as typed records.** `pause_reason` and `pause_code` become one `pause` object with a `cause` and a `reason`, and a node's `handoff` is the handoff record rather than its fenced text. A paused graph saved by an earlier release must be resumed or cancelled before upgrading. (#3168)
