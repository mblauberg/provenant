## Fixes

### Lanes and settlement

- A writer that commits its work and then repeats its handoff block settles as one declaration with a warning: identical copies are accepted and differing copies settle on the last. A final `json`-fenced schema-v2 handoff is accepted with a warning when no `handoff` fence ends the answer, while a `json` block followed by more content stays ordinary text. (#3341)
- A writer that exits without a handoff keeps its uncommitted paths and pinned head in the terminal reason and sealed writer terminal. An empty final message is recorded as the `empty_result` cause appended to that reason, never substituted for the path list, leaving the graph continuable for the chair. (#3341)
