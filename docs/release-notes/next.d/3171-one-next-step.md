## Breaking changes
### Pipelines and graphs

- **One rule decides what follows a stopped lane.** A graph node that stops is restarted in place on its worktree, retried once on its route for a transient failure, handed to another route at once when its route is out of quota, model or login, or returned to the chair, as the stop table in the run spec says; a node's `on_stop` overrides an unpinned cause. A lost or unsettled lane always returns to the chair. Every restart spends the graph's `limits.continuations`. Standalone runs no longer continue themselves, `auto_continue_no_work` is replaced by `on_stop = { stopped_early = "chair" }`, and an honest stop no longer adds a `--continue` node. (#3171)
