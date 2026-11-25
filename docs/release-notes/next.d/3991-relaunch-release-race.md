## Fixes

### Lanes and settlement

- Relaunch in a just-stopped worktree no longer races its release. `provenant stop` acknowledges a cancellation as soon as the stop intent is durable, while the guardian still publishes the terminal record and clears the worktree's writer reservation afterwards. A launch into that worktree now waits, within the store's sourced lock budget, when the slot shows a matching durable stop intent or a release already in progress, and wakes as soon as `writer.lock` is free. Graph launches classify a settling writer lease as bounded contention and report a retry wake instead of settling `blocked` immediately. A live holder without release evidence still refuses at the first probe, so another lane's uncommitted work is never adopted. (#3991)
