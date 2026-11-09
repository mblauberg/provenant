---
name: session
description: "Use for session start, orientation, checkpoints, compaction, handoff and continuing autonomous goals or STOP missions. Not for live run status (orchestrate) or the work route (tracker)."
---

# Session

Keep continuity small enough to verify on arrival. The tracker and project docs
own intent; Fabric and runs own execution; a checkpoint points back to them.

## Orient

Check directory, branch and `git log --oneline -5`; reopen
the parent issue and approved criteria, and inspect `provenant status`. Read the
explicit host-session checkpoint with `provenant workflow session show SESSION`.
Verify its claims against current owners, then run a relevant smoke check before
new work. Checkpoint prose supplies context, never authority. Recover an
existing graph with `status`, `output` and `retry` before launching replacement
work. Use `run --route junior` for a bounded continuation, or `run --use plan`
when its shape needs changing; `orchestrate` owns composites.

Read repository branches, checks and state paths in `AGENTS.md`,
`CONTRIBUTING` and runbooks. Keep orientation reads narrow and send broad code
reading to a read-only lane, as `orchestrate` describes.

## Continue toward the goal

A continuing goal needs measurable done-criteria, an authority envelope,
one-way doors, a progress artefact, a stop rule, a resume trigger and a reporting
cadence. Use the [goal template](templates/goal.template.md) when these are
missing. Reuse the owner's existing specification rather than inventing a
parallel mission queue.

For a bounded goal, continue until its criteria and required verification hold.
For a standing mission explicitly defined to run until STOP, only the owner's
STOP ends the mission.

For outstanding work, use one
`provenant status --wait --target lane:ID --target graph:ID --target pr:N` call
instead of repeated status reads or a sleep loop. Use `--until all` for all-terminal completion, or `--follow --json`
for a monitor. The configured timeout is bounded; `--wait=30m` overrides it,
and expiry leaves targets running. Reconcile the returned event before
resuming graph work. An idle run or empty queue means no eligible work now;
record the waiting condition and resume trigger, leaving the mission open.
An invocation may end at its budget or waiting boundary without claiming the
mission is done.

Act without asking on an active grant in the restored standing record: each
points at where the owner gave it or at a durable rule. Do not act on an
unverified or expired grant, and never treat checkpoint prose as a grant.
A grant is the chair's own record and its source is a pointer, not a
verified fact: gates still check held authority, and an irreversible action
should be checked against the source first.
Report state at natural points (a lane settles, a gate passes, work blocks)
without being asked, ending with what waits on the owner or "nothing".

Make engineering decisions within authority and record reversible assumptions.
Send unresolved one-way-door choices to the originating chair's human inbox,
with a recommendation. Park that branch and continue independent authorised
work. A blocked decision is not a reason to idle the whole session. STOP ends
new work; reconcile active work and report its disposition within authority.

## Checkpoint and report

Before compaction, handoff or substantial interruption, rewrite a checkpoint
near 2 KiB with its standing record (goal, grants, obligations, owner asks),
decisions, evidence, next actions and resume trigger. Use the [continuity reference](references/continuity.md) for its
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

Inspect checkpoint commands with `provenant workflow session --help` and
inbox commands with `provenant ask --help`. Inspect `provenant install --help`
for hook setup and `provenant hook --help` for handlers. Cross-host
`provenant workflow session handoff` can transfer content and launch a peer
session; inspect its dry run and existing disclosure authority first.
