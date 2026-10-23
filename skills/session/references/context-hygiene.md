# Context hygiene

Keep the smallest current briefing that sends the next session back to owned
evidence. Project documents and run records own durable truth; a host-keyed
session checkpoint is only a bounded resume aid.

## Context and evidence

- Keep project instructions, approved scope, decisions, and current owner paths
  easy to find. Follow those owners for current status.
- Let the run and Fabric retain task assignments, lane state, outputs, and
  recovery records. Use `provenant lanes`, `provenant output`,
  `provenant reconcile`, and `provenant resume` when continuing work.
- Before compaction or handoff, save concise Markdown for the explicit host
  session ID with `provenant workflow session save <session-id> --file
  <checkpoint.md>`. Include the goal, authority/evidence references, verified
  progress, next actions, blockers, and resume trigger. The checkpoint limit is
  32 KiB; hooks emit at most 8 KiB.
- Do not copy raw transcripts or unreviewed worker claims into the checkpoint.
  Verify progress against live owners after resuming.
- Keep private memory to cross-project preferences. Put project decisions and
  moving status in their project or run owner.

## Retention

Run scratch is settled by the run guardian; prune merged worktrees with
`scripts/worktree remove --merged`. Remove only the exact paths covered by
explicit authority. Never delete unknown, user-owned,
pre-existing, or old-root state, and do not treat a temporary path as disposable
without proving its ownership. A failed or unavailable review leg stays visible
with its reason and evidence pointer.

Do not import legacy mission queues, session files, or run records into the new
workflow. An active task can be resumed by reopening its owner documents and
creating a new checkpoint only when authorised.
