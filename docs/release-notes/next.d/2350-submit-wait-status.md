## Breaking changes

- `run --wait` that ends before the run settles now returns status `waiting` with exit 0, not `timeout` with exit 124. The lanes keep running, and the human line names the command that keeps waiting (#2350).
