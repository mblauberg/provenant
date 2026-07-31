---
name: session
description: "Use for start, checkpoint, handoff, compaction, or end-of-session continuity on substantial work. Not for a multi-session route map or read-only write authority; use work-map for effort state."
---

# Session

Store continuity in the project owner; without write authority, propose a chat
delta. Project instructions may override continuity paths.
Fallbacks:
state `docs/STATE.md` (about 120 lines), handoffs `docs/handoffs/`, archive
`docs/archive/`. Instructions may override `STATE_FILE`, `HANDOFF_DIR` and
`ARCHIVE_DIR`.

## Start

For substantial work, start at the approved phase/slice and reopen disk state;
never trust injected state. Resume from the digest-bound handoff, reading only
relevant docs/open decisions. User gates stay unanswered until decided.
Routine bounded work may continue with context inside authority.

## Checkpoint

Before compaction or handoff, update the canonical handoff when authorised;
otherwise return it without writing. Use
`HANDOFF-YYYY-MM-DD-<slug>.md` with:

- `Status: active`, effort/leg IDs, superseded path, `Consumed-at: pending`;
- original goal, disk-backed progress paths/commits, ordered remainder;
- invariants and exact verification commands.

Keep at most one active handoff per effort/leg. A fresh session resumes from it.
In the same update, archive a consumed handoff, mark it consumed/time-stamped
and index it; never delete it. Update `work-map` only when the
durable route changes; live state belongs to the work tracker.

Before checkpoint load [context-hygiene.md](references/context-hygiene.md). Run
its read-only audit when run directories, logs, handoffs or large agent-facing
docs accumulate. Consolidate state; never paste transcripts into handoffs.

Provider session retention is minimal: contract-required identifiers,
generation/callback state and resumable digests only. Never retain credentials
or raw transcripts as continuity state. After compaction, revalidate generation,
expiry and ownership before reuse.

## End after changed state

1. **Graduate:** merge surviving behaviour-changing knowledge into its owner:
   decision -> spec/ADR; domain fact -> context/README; convention -> project
   `AGENTS.md`; moving status -> state. Reconcile contradictions, mark
   supersession, refresh timestamps, archive over-cap history; never duplicate.
2. **Close context:** retain minimal manifest, synthesis, verification and
   failure receipts; archive consumed records. Remove only run-owned,
   manifest-classified ephemeral files after proving no live pointer needs them.
   Never delete unknown, pre-existing or user-owned untracked files. Revalidate
   time-sensitive memory against its owning source or mark it stale.
3. **Handoff version control:** run project checks; report the exact diff.
   Commit only with user/project authority; never commit another actor's state.
4. **Signal:** capture only a compact friction pointer in the handoff/state when
   it may recur. `retrospect` owns analysis and process changes after a completed
   cycle; session closure does not start a mini-retrospective.

Periodic hygiene is opt-in; record owner, cadence, scope, resource cap, last
success and disable condition. It may audit/archive classified artifacts and
refresh indexes, but not commit, deploy, communicate or delete
unknown files. Staleness becomes visible state, not catch-up churn.

Put project knowledge in project docs; follow
[context-hygiene.md](references/context-hygiene.md) for lightweight private memory.

## Adapter-absent path

Without adapters, emit the skill-owned kind in
[portable-workflow.v1.json](portable-workflow.v1.json). Validate
`accepted_artifact_identity`. Output proves context only, not handoff or task
truth. Keep canonical context separate.
