## Breaking changes
### Lanes and settlement

Lanes now use one schema v2 `handoff` JSON fence for completion, partial
finishes and continuation. The `Status: complete|incomplete` line and schema
v1 handoffs are no longer recognised. The schema requires `status` (`done`,
`stopped_early` or `blocked`), `note`, `remaining`, `decisions` and `verify`.

Lanes keep bounded `handoff.json` progress outside the worktree. If a provider
stops without a final answer, recovery reads the applied lane's progress and
carries it into the next attempt, including a fresh account continuation.
Fresh manual continuations use the handoff instead of a prior result tail.
