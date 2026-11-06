## Breaking changes
### Pipelines and graphs

- **Start lanes with `run` and templates.** `dispatch`, MCP `submit` and bundled presets have been removed. Use `run --mode worktree-write --worktree PATH` for an existing writer, or `run --branch NAME --base REF` for a new branch. Retained non-graph lanes remain readable through `status` and `output`; stop and retry require a graph. Drain live lanes and pending graphs before upgrading, and port user presets to templates by hand (#2867).
