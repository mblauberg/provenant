# Operating autopilot from Codex

Use the Codex session as the mission conductor while the run and Fabric own
live assignments, lanes, outputs, and recovery. `orchestrate` owns routing and
bounded agent waves; Codex does not create a second queue, lease, or external
loop driver.

## Work and recovery

- Read the user-owned goal, current scope, and authority before acting. Check
  `STATUS` at the start and end of each work iteration.
- Use `provenant lanes` and Fabric tasks to see what is active, ready, blocked,
  or complete. Dispatch bounded work with `fabric_submit` through
  `orchestrate`; read results with `provenant output`.
- After an interruption, run `provenant reconcile --id RUN`, inspect the exact
  task and artifact, and resume the exact attempt with `fabric_run`'s `resume`
  operation (`target: { attempt }`, `expected_generation: 0`, `operation_id`,
  `prompt`). `expected_generation` counts earlier continuations, so pass 0; it
  is not the attempt record's `generation`. Preserve partial work and retry
  only missing or failed tasks within their owner's retry bounds.
- Use native subagents for independent work when the governing orchestration
  contract allows it. Preserve task scope, write authority, artifact paths,
  checks, and return format. Synthesis and acceptance stay with the accountable
  owner.
- Route provider and model choices through `orchestrate`. Record only the
  adapter/model/effort supplied by the actual launch or attempt receipt; never
  infer a route from memory or model spelling.

## Session continuity

Before compaction or a substantial handoff, save bounded Markdown for the
actual host session:

```sh
provenant workflow session save <session-id> --file <checkpoint.md>
```

The checkpoint records the goal, authority and evidence references, verified
progress, next actions, blockers, and resume trigger. SessionStart injects only
that session key's checkpoint or an explicit missing/stale status. PreCompact
reports whether it needs saving. The checkpoint is capped at 32 KiB and hook
output at 8 KiB. Reopen live task records and verify cited work after resume.

## Idle and STOP

If the run has no eligible or active work, follow its admission and pause
behavior, report the resume trigger, and end this invocation. Do not infer
mission completion from an empty lane list. The mission stays open until the
user sets `STATUS: STOP`; only then reconcile current work and provide the
final handoff.
