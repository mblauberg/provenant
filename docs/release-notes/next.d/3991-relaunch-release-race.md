# Relaunch in a just-stopped worktree no longer races its release (#3991)

`provenant stop` acknowledges a cancellation as soon as the stop intent is durable; the guardian
still publishes the terminal record and clears the worktree's writer reservation afterwards. A
launch into that worktree now waits, within the store's sourced lock budget, when the slot shows a
release already in progress, and wakes as soon as `writer.lock` is free. Graph launches classify
a settling writer lease as bounded contention and report a retry wake instead of settling
`blocked` immediately. A live holder from another lane still refuses at the first probe, so
another lane's uncommitted work is never adopted.
