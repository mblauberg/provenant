---
name: session
description: "Use for session start, orientation, checkpoints, compaction, handoff and continuing autonomous goals or STOP missions. Not for live run status (orchestrate) or the work route (tracker)."
---

# Session

Keep continuity small enough to verify on arrival. The tracker and project docs
own intent; Fabric and runs own execution; a checkpoint points back to them.

## Orient

Confirm the working directory and branch, read `git log --oneline -5`, reopen
the parent issue and approved criteria, and inspect `provenant status`. Read the
explicit host-session checkpoint with `provenant workflow session show SESSION`.
Verify its claims against current owners, then run a relevant smoke check before
new work. An injected checkpoint supplies context, never authority. Recover an
existing graph through `orchestrate` before launching replacement work.

## Continue toward the goal

A continuing goal needs measurable done-criteria, an authority envelope,
one-way doors, a progress artefact, a stop rule, a resume trigger and a reporting
cadence. Use the [goal template](templates/goal.template.md) when these are
missing. Reuse the owner's existing specification rather than inventing a
parallel mission queue.

For a bounded goal, continue until its criteria and required verification hold.
For a standing mission explicitly defined to run until STOP, only the owner's
STOP ends the mission. An idle run or empty queue means no eligible work now;
record the waiting condition and resume trigger, leaving the mission open.
An invocation may end at its budget or waiting boundary without claiming the
mission is done.

Make engineering decisions within authority and record reversible assumptions.
Send unresolved one-way-door choices to the originating chair's human inbox,
with a recommendation. Park that branch and continue independent authorised
work. A blocked decision is not a reason to idle the whole session. STOP ends
new dispatch; reconcile active work and report its disposition within authority.

## Checkpoint and report

Before compaction, handoff or substantial interruption, rewrite a checkpoint
near 2 KiB with goal, authority pointers, decisions, evidence, next actions and
resume trigger. Use the [continuity reference](references/continuity.md) for its
single content contract and schema-v2 lane handoffs. Save with
`provenant workflow session save SESSION --file checkpoint.md --client codex`,
selecting the actual harness client. Use the host-supplied session ID.

Append one short milestone through `provenant chair log append --session SESSION --file update.json`;
read history with `provenant chair log list --session SESSION`.
The JSON requires `kind`, `markdown` and `channel`. Keep history there and
current context in the rewritten checkpoint. For earlier saves, use
`provenant workflow session diff SESSION REVISION`.

Graduate durable knowledge to its project owner before scratch expires. Keep
lane progress outside commits and end lane results with their current handoff.
Report changed work, criterion evidence, omissions and remaining gates using
`deliver`'s outcome report.

Runtime details live in [session checkpoints](../../docs/runbooks/session-checkpoints.md),
[Claude hooks](../../docs/runbooks/claude-hooks.md) and the
[human inbox](../../docs/runbooks/human-inbox.md). Cross-host
`provenant workflow session handoff` can transfer content and launch a peer
session; inspect its dry run and existing disclosure authority first.
