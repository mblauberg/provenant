# State contract: the durable spine of a mission

> **context-window = RAM, filesystem = disk.** A multi-week mission will be
> summarized, compacted, crash, or hit transient failures many times. Nothing
> in the conversation survives. The conductor does no deep work in its own
> context — it delegates, persists every meaningful result to a file, and
> keeps only a one-line pointer in-head. A fresh session (or a user) must be
> able to resume from the files alone.

*Implemented by `templates/GOAL.template.md`, `templates/STATE.template.md`,
`templates/HANDOFF.template.md`, and `scripts/bootstrap-autopilot.sh` (which
scaffolds the file set and substitutes the knobs below).*

## Where mission state lives

The skill directory ships templates and scripts only — it is never written
to at runtime. Every run's state lives under **`.agent-run/runs/<mission-run-dir>/`**,
the same session-owned run location `deliver`, `implement` and `orchestrate`
use for their receipts (`RUN.json`) and run directories. `bootstrap-autopilot.sh
<mission-id>` creates and fills it. This keeps a mission's durable memory
outside the skill, alongside any `RUN.json` a delegated `deliver`/`implement`
stage writes into the same mission directory.

## The file set (fixed shape; names/knobs at bootstrap)

| File | Owner | Mutability | Role |
|---|---|---|---|
| `GOAL.md` | **user** | edited by user only | North star (`{{MISSION}}`) + the `STATUS: RUN/STOP` gate + `{{LOCKED_CONSTRAINTS}}` + steering directives |
| `STATE.md` | conductor | **rewritten every iteration** | The heartbeat + the single recover-after-compaction anchor |
| `QUEUE.md` | conductor | rewritten per iteration | The durable **work queue and item-lease ledger** — one row per unit: `id / status / depends-on / lease-owner / lease-expiry`. `PENDING` = selectable, `LEASED` = in-flight (record-before-launch), `DONE`, `BLOCKED`, `DEFERRED`. |
| `HANDOFF.md` | conductor | regenerated on material change | Capstone synthesis + terminal pickup, refreshed before STOP |

**The load-bearing invariant:** no single file is both the steering input and
the authoritative record. GOAL steers, STATE remembers, QUEUE tracks work and
leases. If you find yourself authoring mission content into GOAL, or leaving
a lease row stale after a job finishes, you have broken the contract.

## What this layer does NOT own

The gutted lab used to also own a flat `adr/<id>.md` decision archive, cross-
family review sidecars, dashboards and immutability tooling. That doctrine
now belongs to the skills that already own it, not a forked copy here:

- **Decisions/ADRs** — when a work unit reaches a decision worth a durable
  record, delegate it to the owning run's `implement`/`deliver` lifecycle;
  its receipt lands at `.agent-run/runs/<mission-run-dir>/RUN.json` per their
  `run-contract.md`/`contract.md`. Do not fork a second ADR tree here.
- **Model/effort routing** — owned by `orchestrate`; every wave dispatched
  from this mission goes through it rather than a lab-local matrix.
- **Cross-family review capture and the reviewed-artifact path** — see
  `cross-family-review.md`.

## Record-before-launch + RECONCILE-first

Before dispatching any background job, write its `QUEUE.md` row to `LEASED`
with `lease-owner` and `lease-expiry` filled — **then** launch. Every
iteration begins (RECONCILE) by reading `QUEUE.md`'s leased rows: completed
→ record the result and flip to `DONE`; dead/expired → re-dispatch or mark
`BLOCKED`. Clear a lease only here, never at launch. This is the mechanic
that survives a crash or mid-flight compaction — see
`recovery-and-cadence.md` for the full protocol.

## STOP and closure

Flipping `GOAL.md`'s `STATUS` to `STOP` requires `GOAL` + `STATE` + `HANDOFF`
to agree on the terminal truth — a `STOP` written while `HANDOFF.md` is stale
is a finish-blocker. `STATE.md` and `QUEUE.md` must show zero leased rows and
an empty selectable frontier before a `PAUSED — reason: idle-frontier`
checkpoint is valid; only a user `STATUS: STOP` closes the mission.
