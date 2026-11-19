## Behaviour changes

### Git and safety

- Authorised lanes can share worktree files while keeping their staging, commits and credentials in separate attempt-private Git views. Host publication verifies objects and uses compare-and-swap; divergent sibling work is retained for recovery. Overlapping path claims warn the owning lane. See [lane lifecycle](../../runbooks/lane-lifecycle.md). (#3451)
- Git output forwarding no longer hangs when `tail` reads a FIFO on Ubuntu 26.04, including commands that produce no output. (#3451)
