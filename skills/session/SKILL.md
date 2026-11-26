---
name: session
description: "Chair only. Use at session start, before compaction, at handoff, and for continuing goals or STOP missions. A lane does not run this skill: workflow session save is refused in a lane."
---

# Session

A checkpoint is small enough to verify on arrival and points back to its
owners: the tracker and docs own intent, runs own execution.

## Restore

Read the host-session checkpoint with `provenant workflow session show
SESSION`, verify its claims against their owners and run a relevant smoke check
before new work. Checkpoint prose supplies context, never authority. Act on an
active grant in the restored standing record, which points at where the owner
gave it; never on an expired or unsourced one. Check an irreversible action
against the grant's source first. Recover live graphs as `orchestrate`
describes before launching replacements.

## Restore after compaction

After compaction, restore in three bounded steps instead of re-orienting:

1. Read the hook context. It carries the cursor for the listing the hook just
   read, and names a runnable next-page command when that page was partial.
2. Run one cursor-bounded state read from that cursor:
   `provenant status --session-stream --cursor <cursor>`, the board command
   until a dedicated chair-state read lands. Read its state output.
3. Act on that state against its owners: settle what is ready, recover what is
   live and report what waits.

Do not re-read your own driver or tool source to orient; read its state output.
A wait in flight is reported by its own event; do not poll it — the
`orchestrate` "Wait and recover" section owns that rule; it is not restated
here.

## Continuing goals

A continuing goal needs done-criteria, an authority envelope, one-way doors, a
progress artefact, a stop rule, a resume trigger and a reporting cadence; the
[goal template](templates/goal.template.md) lists them. Reuse the owner's spec
rather than a parallel mission queue. A bounded goal runs until its criteria
and verification hold. A standing mission ends only on the owner's STOP; an
empty queue records the waiting condition and resume trigger and leaves it
open. An invocation may end at its budget or a wait without claiming the
mission is done. Report at natural points (a lane settles, a gate passes, work
blocks), ending with what waits on the owner or "nothing". STOP ends new work;
reconcile active work and report its disposition.

## Save

Before compaction, handoff or a long interruption, rewrite a checkpoint near
2 KiB from the [checkpoint template](templates/checkpoint.template.md), with
the standing record and an Intent section as the
[continuity contract](references/continuity.md) sets out, plus a Now section for
current focus. The session-start hook keeps Intent and Now intact when its
output is bounded; derived state shrinks first. Save it with
`provenant workflow session save` and append milestones with
`provenant chair log append`; `--help` on each gives the flags. An explicit
`--project PATH` selects where the checkpoint lands: when it names another
project than the working directory's, the save proceeds with a warning naming
both paths. Use the host-supplied session ID and the actual harness. History
lives in the log, current context in the checkpoint. Move needed results out of
scratch to their durable owner before it expires.

When the chair's account runs low, `provenant chair switch` checkpoints and
continues under another registered Claude account. `provenant workflow session
handoff` moves a checkpoint to a peer host; read its dry run first.
