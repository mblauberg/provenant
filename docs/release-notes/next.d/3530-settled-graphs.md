## Fixes

### Lanes and settlement

- A graph whose nodes have all settled and whose worktree is gone with no live
  presence or lease settles itself on the next tick: succeeded when its outcome
  succeeded, otherwise stopped with the reason "worktree removed after
  settlement". (#3530)
- `gc` warns about paused or pending graphs older than `gc.stale_graph_age`
  (default `1d`, source shown) whose branch has merged or whose PR is closed;
  `gc --apply` stops them. (#3530)
- `provenant status` lists live lanes first and counts settled-but-open graphs
  in one summary line; `--full` lists them. (#3530)
