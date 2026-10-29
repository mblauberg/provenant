## Additions
### Lanes and settlement

- A lane that finishes only part of its brief is now flagged instead of reading as complete. An attempt carries `partial_signals` when its final answer ends with a valid `stopped_early` `handoff` block, the provider reports a stop other than a finished turn, or a writer left uncommitted edits, and `lanes` shows `(partial)` with a resume hint (#2178).
