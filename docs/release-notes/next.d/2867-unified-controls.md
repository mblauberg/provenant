## Breaking changes
### Lanes and settlement

- Use `status [ID]` instead of `lanes` or `fabric graph status`, `stop ID` instead of `cancel`, and `retry ID` instead of `resume`. Status rows share one schema for graphs, nodes and attempts, and output and steering accept each ID kind (#2867).
- `retry` can recover a graph with `--extend`, or continue an attempt with its retained brief or `--brief`. Graph subtree stop, node redo and brief replacement still require kernel support and are refused before changing the checkpoint (#2867).
