## Fixes

### Pipelines and graphs
- A lane's outcome now comes from observed refs and its sealed handoff, not from the stop path that reported it. Commits ahead of the node's input head count whether or not they were pushed, and a `done` handoff closes the brief, so a writer that committed and pushed with a done handoff no longer reports `review_required` with `outcome=declared_stop` and `cause=stopped_no_work`. (#3988)
- A stop with new commits records `writer_declared_stop`; `stopped_no_work` is reserved for a stop that left no new commit.
