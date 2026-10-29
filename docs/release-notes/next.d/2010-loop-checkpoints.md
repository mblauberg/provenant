## Breaking changes
### Pipelines and graphs

- **Graph checkpoints that hold loop decisions from 1.2 or earlier no longer advance.** Loop decisions now live in the checkpoint's node asks, and older checkpoints are not migrated. `provenant fabric graph run` and `graph resume` refuse such a checkpoint with a message naming the next step: cancel it with `provenant fabric graph cancel <id>`, which still works, then rerun the document under a new `graph_id` (#2010).
