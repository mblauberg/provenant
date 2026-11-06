# Brief and collect

Use a [template](../../../docs/runbooks/presets.md) before creating a custom
composite. Give each assignment outcome, acceptance evidence, source SHA,
read roots, literal write scope, authority, budget, output location and stop
conditions. The [planner brief](planner.md) owns slicing and tier choice.

Writers commit recoverable steps, run targeted checks and report environment
failures without repairing unrelated code. Name one writer per worktree;
confirm a stopped process has exited before adopting its worktree. Reviewers
receive the base, pinned subject and spec without the author's hypothesis.

```sh
provenant run --use task --file brief.md --explain
provenant status
provenant output TARGET
provenant output TARGET --trace
provenant retry TARGET
provenant stop TARGET
```

Submit without `--explain` after checking the shape. Retain graph, node and
attempt IDs. A wait expiry leaves work running. Reconcile recorded state before
retrying; a retry can continue unfinished work or redo a target within authority.
Use `edit` to patch unstarted work with an expected revision and idempotency key.
Started attempts are immutable. Keep the approved budget and host limits.

Collect the result and evidence paths; check commits, paths, acceptance evidence,
review subject and resolved route. Integrate disjoint heads in dependency order,
run affected checks and review the combined head. A failed child blocks dependent
work; synthesis cannot waive a gate. Land only with the repository's authority
and exact-head checks, using `provenant land N --head SHA --wait`.

For a ready-issue queue, admit only approved work with stable acceptance,
satisfied dependencies and disjoint scopes. Cap active work by resources and
useful independence. Tracker owns readiness, runs own execution and session
owns milestones and stop rules; keep no second board.
