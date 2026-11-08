## Fixes
### Lanes and settlement

- A Claude lane whose final result event is empty, such as when its last turn was a tool call, now keeps its last non-empty assistant message as the result and records that in the settlement reason. A writer that exits cleanly with no final message at all settles as `review_required` with its commit kept, not `task_failed`. The "findings block missing or invalid" warning no longer appears on nodes that neither require findings nor review; review and findings-required nodes still fail on a missing block.
