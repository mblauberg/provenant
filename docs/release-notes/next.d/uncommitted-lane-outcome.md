## Behaviour changes

- A writer lane that succeeds but leaves uncommitted edits and no new commit
  now ends `uncommitted` instead of `succeeded`. The reason names the dirty
  paths; ignored paths and `.review-snapshots/` do not count. The outcome
  shows in `lanes`, the `dispatch` summary line and the attempt facts, and
  `output` reports the lane's `state` and `reason`. Host delivery does not
  run for it, and a graph node ends failed with no route fallback (#1740).
