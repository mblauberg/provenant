# Operating loop

Autopilot keeps a long-running mission within the user's goal and authority.
The run and Fabric own live tasks, lanes, execution state, admission, and
recovery. This reference describes how the conductor uses those owners; it
does not define another queue or scheduler.

## Each iteration

1. Read the user-owned goal and check `STATUS`. Reopen current scope,
   authority, and relevant project owners.
2. After an interruption or uncertain task state, run
   `provenant reconcile` before making new assignments.
3. Inspect `provenant lanes` and the mission's Fabric tasks. Confirm which
   outputs are complete, active, blocked, or awaiting a user decision.
4. Select work that is within the approved goal and build ceiling. Keep
   independent tasks moving when one item is blocked by a gate.
5. Dispatch bounded waves through `fabric_run` (`op: submit`) and `orchestrate`.
   `orchestrate` owns provider routing, topology, coordination, and review.
   Fabric tasks carry assignments and answer-bearing results.
6. Read results with `provenant output`. Verify the named artifacts and
   required checks, then route accepted changes through `implement` or
   `deliver` when their lifecycle applies.
7. Escalate unresolved choices or unavailable work with the evidence and
   decision needed. Follow the owning skill's retry bounds; do not loop on
   unchanged failures.
8. Save a host-keyed session checkpoint before compaction or substantial
   handoff when continuity would help:
   `provenant workflow session save <session-id> --file <checkpoint.md>`.
   Keep goal, direction, process, and plan with a resume trigger, near 2 KiB.
   Record milestones in the chair log with `provenant chair log append`.
9. Check `STATUS` again before ending. If the run has no eligible or active
   work, follow run admission and pause behaviour, state what would resume it,
   and end this invocation. Idle is a waiting condition, never mission
   completion. Only the user sets `STATUS: STOP`.

## Evidence and delegation

Delegate independent work through `orchestrate`; keep synthesis and
adjudication with the accountable stage owner. Treat a worker's verdict as a
claim until the artifact and required checks support it. Preserve failed or
unavailable review legs with their reason. Decisions that need a durable
record go to the existing project owner.

A checkpoint is navigation context, not proof of completion or permission.
After `fabric_run` `resume`, reconcile live tasks and reopen the cited evidence.
Do not repeat completed work just because a new session lacks its transcript.
