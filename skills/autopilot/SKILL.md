---
name: autopilot
description: "Use for a large, open-ended mission that must survive sessions/crashes and run user-out-of-loop until STOP; delegates lifecycle work to other skills. Not for pre-scoped delivery that stops at a PR gate; use orchestrate's autonomous mode or deliver."
---

# Autopilot

A thin autonomous **conductor** for large missions: autonomous scoping,
self-drive, durable cross-session state/recovery, and a hard user `STOP`
gate. It owns no lifecycle machinery itself; it delegates every bounded wave
to `orchestrate` and every decision/software change to `implement`/`deliver`,
then records the durable pointers.

It is **higher-authority** than `orchestrate`'s pre-scoped
autonomous-implementation mode, which stops at a user PR gate; autopilot
keeps going until a user writes `STATUS: STOP`.

Claude Code and Codex are equal operators; Codex operators read
[codex-operator.md](references/codex-operator.md) first.

## Entry gate

Needs all of: an open-ended mission too large for one bounded wave, a
survivable-crash requirement, and an explicit user STOP gate as the only
clean exit; otherwise use a lighter-weight skill.

## Bootstrap

1. Run `scripts/bootstrap-autopilot.sh <mission-id>`, fill every CONFIG KNOB,
   then rerun to substitute/validate. See
   [state-contract.md](references/state-contract.md) for the file set and
   `.agent-run/<mission-id>/` location.
2. Use `orchestrate` for every bounded wave: it owns topology, provider
   routing, coordination, the review ladder and degradation.
   Delegate decisions/software change to `implement`/`deliver` and record
   the returned route/result in `QUEUE.md`.
3. Operate one iteration at a time after reading
   [operating-loop.md](references/operating-loop.md),
   [state-contract.md](references/state-contract.md), `GOAL.md`, `STATE.md`,
   `QUEUE.md`'s head. Keep one conductor lease.
4. Interactive missions use Herdr and record owned panes. External-driver-only
   missions record `HERDR-NOT-USED: external driver; filesystem state is
   authoritative` in the run-local `STATE.md`.

Bootstrap authorises only mission-declared isolation. Source-repository
implementation branches and linked worktrees are covered by standing authority
and use `provenant worktree`; project restrictions, other worktrees and
destructive cleanup remain separately gated.

## Operating loop

Run `RECONCILE -> READ -> SELECT -> DISPATCH -> RECORD -> PROPAGATE ->
REORG-if-due -> STATE -> WAKE/STOP`. Flip a `QUEUE.md` row to `LEASED` before
launch. Delegate deep work; fan out independent contexts and serialise shared
state. Bound retries; escalate stalls with evidence. An empty queue triggers
one bounded re-enumeration pass, then an idle checkpoint and paused dispatch;
only user STOP closes the mission. See
[operating-loop.md](references/operating-loop.md) and
[recovery-and-cadence.md](references/recovery-and-cadence.md) for detail.

## Evidence and closure

The run-local `STATE.md` holds current recovery truth; `QUEUE.md` is the durable work queue
and item-lease ledger. Durable decisions are delegated, not forked here; see
[state-contract.md](references/state-contract.md).

Cleanup must classify first: never delete unknown files; prune only
mission-owned, manifest-classified ephemeral payload with no live reference.
STOP requires `GOAL`/`STATE`/`HANDOFF` agreement and reconciled, closed work;
see [recovery-and-cadence.md](references/recovery-and-cadence.md) for the
closure protocol.

## References

Load one relevant reference: [state-contract.md](references/state-contract.md),
[cross-family-review.md](references/cross-family-review.md), or the
loop/recovery/Codex files above.
