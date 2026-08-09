# Orchestration contract (substrate-neutral)

Any orchestrator — Claude Code, Codex, Cursor, or a future substrate — that
drives multi-agent work through this skill's adaptive loop realises the same
stage/gate/recovery graph. This file is the canonical, substrate-neutral
description of that graph, and is this skill's **public interface**: external
consumers (`codebase-polish`, `cross-verify`, and any future skill outside
this skill's boundary) should bind to this contract, not to one substrate's
private implementation notes.

Two current adapters realise this contract:

- **Claude Code Dynamic Workflows** —
  [dynamic-workflows.md](dynamic-workflows.md) binds the graph to a
  JavaScript `Workflow()` runtime.
- **Codex / Cursor native subagents** —
  [codex-subagents.md](codex-subagents.md) binds the graph to native
  multi-agent collaboration, or explicit waves at lower efforts.

A new substrate binds to this contract the same way: implement the stages,
gates and recovery transitions below, then add a short adapter note analogous
to the two above. Nothing in this file is Claude-specific; runtime facts
(concurrency limits, session mechanics, saved-workflow syntax, native-subagent
role names) live only in the adapters.

## Stages

1. **Bootstrap** — resolve a run directory, discover repo/project
   conventions, and resolve model routes for each role
   (`routing-and-tiers.md`).
2. **Scan / Decompose** — bounded fan-out into independent, atomic
   candidates or claims; workers write full detail to run-dir files and
   return headlines only.
3. **Review / Verify** — per-candidate parallel review, including at least
   one cross-family pass where data policy and tooling allow
   (`verification.md`).
4. **Adjudicate / Synthesise** — one reducer (never a vote) collapses all
   candidate reviews into decisions or a report; objective checks and
   protected-path rules outrank opinion.
5. **Apply / Escalate** — a single serial applier lands only low-risk,
   objectively-gated changes; everything else exits as a patch or finding
   plus a written recommendation. This is the escalation boundary, not a
   pause.
6. **Adaptive waves** — after any reduce step, choose `continue`, `narrow`,
   `repair`, `verify`, `document`, or `stop`; do not force every run into one
   fixed sequence.

## Gates

- **Stage validation gate** — a schema or objective check (test, lint,
  source existence) before a stage's output feeds the next stage.
- **Escalation boundary** — high-risk, uncertain, or protected-path items
  exit as patches/findings with rationale; they are never auto-applied and
  never silently dropped.
- **User gate** — the run stops at explicit user approval, acceptance, or
  merge; a gate-adjacent stage ends there and records `awaiting-user` rather
  than blocking on it. How a given substrate enacts that stop-and-record (live
  process pause, script termination, new invocation to resume) is adapter
  mechanics, not part of this contract — see `dynamic-workflows.md` and
  `codex-subagents.md`.
- **Final gate** — no untriaged P0/P1, no missing anchors, no unresolved doc
  drift, and family/cross-family status recorded
  (`CROSS-FAMILY-NOT-RUN: <reason>` where applicable).

## Recovery

- The **run directory** (`findings/`, `crossfamily/`, `traces/`, `patches/`,
  plus a manifest/receipt) is the durable ledger. Any adapter, on any
  substrate, resumes from these files rather than from conversation state.
- **Chair/driver loss** — persist a handoff; only a generation-bound
  takeover promotes a new chair, never a silent promotion.
- **Worker/peer loss** — preserve partial output, mark it degraded, and
  reassign only if authority and review independence still hold. A failed,
  missing, or null result never proves that the worker made no partial write.
- **Cross-family failure** — record `CROSS-FAMILY-NOT-RUN: <reason>`; never
  substitute a same-family answer and call it cross-family.
- **Tool/auth failure** — log the error to the run scratchpad and advance to
  the next entry/tool; never silently skip a verification step.

## Worker contract

Every dispatched worker states task class, route tier, catalog model identity,
effort, route receipt, identity, objective, authority, paths, output, checks,
stop condition and budget. A worker that changes code or other observable
behaviour also names its governing skill (for example, `tdd` under
`implement`); the binding is invalid when that field is absent or ambiguous.
The binding is invalid if any route field is absent;
validate payloads and never infer permission. Adapters configured for
account-default transport omit a literal model while their receipts retain
catalog identity and effective effort. Claude records the runtime-discovered effective model and the admitted
probed effort, marked `provider-unverified`; its canary cannot observe effective
effort.
Forbid unpartitioned edits and out-of-scope git restore/checkout/stash. Stop
at budget or invariant failure and record residual work. Handoffs preserve
claim, source, confidence, issues and validation. Certification needs a
non-authoring reviewer plus verified evidence; best-effort routes only scout,
they do not certify.

A worker's final act before idling is its report: the digest plus artifact
path through its return channel or a direct message to the chair, or an
explicit failure report. Going idle without one is a contract breach, not a
neutral state. A chair observing idle-without-report verifies the deliverable
directly from the filesystem, then queries the worker once; completion is
judged from the verified artifact, never inferred from silence.

## Non-goals

This contract does not prescribe a topology (single chair, paired-primary, or
a run-until-STOP lab loop — see `paired-primary.md` and `autopilot`),
a specific model/tier (`routing-and-tiers.md`), or a specific cross-family
dispatcher (`cli-headless.md`). It fixes only the stage/gate/recovery shape
every adapter must realise, so an external consumer can depend on the shape
without depending on one substrate's syntax.
