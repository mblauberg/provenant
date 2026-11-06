## Behaviour changes
### Pipelines and graphs

- Child runs can opt out of parent and graph cancellation with `run --detached`. Lanes can stop their own subtrees. A parent that finishes closes its live attached children; work emitted in its final planner patch remains scheduled.
- Waiting parents release writer admission and graph concurrency so children can drain within the configured limits.
- Integration into a checked-out parent branch now waits for explicit pickup, preserving the parent’s index and edits. Commit the parent’s work and merge the sealed child commit, or switch the parent checkout away from the target branch and retry delivery.
