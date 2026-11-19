## Fixes

### Lanes and settlement

- A writer that exits without a handoff keeps its uncommitted paths and pinned head in the terminal reason and sealed writer terminal. An empty final message is recorded as the `empty_result` cause appended to that reason, never substituted for the path list, leaving the graph continuable for the chair. (#3341)
