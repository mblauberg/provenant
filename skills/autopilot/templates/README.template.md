# Autopilot mission: <short name>

## Mission

<One paragraph with the user-owned goal, scope, authority, and STOP condition.>

## Live work

Use `provenant lanes` to inspect execution and Fabric tasks to follow
assignments and results. Use `provenant output` to inspect task artifacts. Do
not maintain a parallel queue or lease ledger.

## Resume and recovery

After interruption, run `provenant reconcile --id RUN`, inspect the existing
tasks and outputs, and resume the exact attempt with
`provenant resume RUN/TASK/ATTEMPT --operation-id ID (--prompt-file PATH |
--prompt-stdin)`. Preserve useful partial work and verify results against
their owners.

## Session checkpoint

Before compaction or a substantial handoff, save a concise Markdown checkpoint
for the actual host session:

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

Keep the goal, authority and evidence references, verified progress, next
actions, blockers, and resume trigger within 32 KiB. Hooks inject only the
checkpoint for that session key, or an explicit missing/stale status.

## User control

Check the user-owned mission goal before and after each iteration. An empty
frontier means waiting; it does not mean completion. Only the user sets
`STATUS: STOP`. When that happens, reconcile work and provide an evidence-linked
handoff with remaining items and their owners.
