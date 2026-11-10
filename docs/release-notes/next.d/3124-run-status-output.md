## Behaviour changes
### CLI and install

- `provenant run` now prints two lines instead of the whole graph document: the graph handle with its state, model and worktree, shown as soon as the checkpoint is saved, then `next:` with the command to run. `--json` and `--full` still return the full document. `provenant status --target ID` no longer needs `--wait` and reports each target's current status once. Lane rows in `status`, `output`, `cancel`, `land` and Fabric results carry one route receipt, `applied_route`; the identical `resolved_route` copy is gone. (#3124)
