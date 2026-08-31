---
name: session
description: "Use for start, checkpoint, handoff, compaction, or end-of-session continuity on substantial work. Not for a multi-session route map; use work-map for effort state."
---

# Session

Read the repository's `Repository process` declaration and its declared
scope/story and workflow-state owners first. If absent, use existing owners or
return a chat handoff; never invent one. Write continuity only to an authorised
handoff or explicit run-local state; without that authority, propose a chat
delta. Updating an external tracker requires external-write authority.
There is no default rolling project state file. Run state remains run-local,
not project-wide truth. Project instructions may override continuity
paths. Fallbacks: handoffs `docs/handoffs/`, archive `docs/archive/`.

## Start

For substantial work, reopen disk state; distrust injected state. Resume using
declared owners and the digest-bound handoff, reading only relevant docs/open
decisions. User gates stay unanswered until decided.
Bounded work may continue within authority.

## Checkpoint

Before compaction or handoff, update the canonical handoff when authorised;
otherwise return it without writing. Use
`HANDOFF-YYYY-MM-DD-<slug>.md` with:

- `Status: active`, effort/leg IDs, superseded path, `Consumed-at: pending`;
- original goal, disk-backed progress paths/commits, ordered remainder;
- invariants and exact verification commands.

Keep one active handoff per effort/leg. Archive a consumed handoff, mark and
index it; never delete it. Update `work-map` only when the durable route
changes; live state belongs to the declared workflow-state owner.

Before checkpoint load [context-hygiene.md](references/context-hygiene.md). Run
its read-only audit when run directories, logs, handoffs or large agent-facing
docs accumulate. Consolidate state; never paste transcripts into handoffs.

Retain only required provider identifiers, generation/callback state and digests.
Never retain credentials or raw transcripts as continuity state. After
compaction, revalidate generation, expiry and ownership before reuse.

## End after changed state

1. **Graduate:** merge surviving behaviour-changing knowledge into its owner:
   decision -> spec/ADR; domain fact -> context/README; convention -> project
   `AGENTS.md`; moving status -> declared workflow-state owner or explicit run
   state, with external writes separately authorised.
   Reconcile contradictions, mark
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
refresh indexes, but not commit, deploy, communicate or delete unknown files.
Staleness stays visible.

Put project knowledge in project docs; follow
[context-hygiene.md](references/context-hygiene.md) for lightweight private memory.

## Adapter-absent path

Without adapters, emit the skill-owned kind in
[portable-workflow.v1.json](portable-workflow.v1.json). Validate
`accepted_artifact_identity`. Output proves context only, not handoff or task
truth. Keep canonical context separate.
