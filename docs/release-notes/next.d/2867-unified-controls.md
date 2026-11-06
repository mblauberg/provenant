## Breaking changes
### Lanes and settlement

- Use `status [ID]` instead of `lanes` or `fabric graph status`, `stop ID` instead of `cancel`, and `retry ID` instead of `resume`. Status rows share one schema for graphs, nodes and attempts, and output and steering accept each ID kind (#2867).
- `stop ID` stops a node and its attached subtree, leaving detached children running. `retry ID --node N [--brief TEXT]` redoes a settled leaf with a fresh attempt identity, retains earlier attempt evidence and budget usage, and journals the brief as a graph patch. `retry` also recovers graphs with `--extend` and continues attempts (#2867).
