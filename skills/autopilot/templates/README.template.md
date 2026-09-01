# Autopilot mission: {{DOMAIN}} — start here

> This file is the **single human entry point**. It never hard-codes a
> status snapshot — those drift. Live state lives in `STATE.md` and
> `QUEUE.md`.

## What this mission is

This is the durable, filesystem-as-memory state for an autonomous mission
over:

> **{{MISSION}}**

A **conductor** drives it: it delegates every bounded wave to `orchestrate`
and every decision/software change to `implement`/`deliver`, records the
durable pointers, and builds real artifacts **up to the build ceiling** —
pausing at **human-in-the-loop (HITL) gates** for the calls only you can
make. It is a continuous process, **run until you write STOP**, not a
finished report.

## Status & what's waiting on you

- **The one switch:** `GOAL.md` → `STATUS` (`RUN` | `STOP`) — the lifecycle
  gate the conductor obeys at the start of every iteration.
- **Live status:** `STATE.md`'s Heartbeat block (rewritten every iteration).
- **Your worklist:** `STATE.md` → **Blockers** and any `BLOCKED` rows in
  `QUEUE.md` — this is the HITL backlog; each item says what it unblocks.

## The read path (resume with zero prior context)

1. **This file** → 2. `GOAL.md` (mission + steering directives + the
   `STATUS` switch) → 3. `STATE.md` (heartbeat + Blockers worklist) →
   4. `QUEUE.md` head (what is selectable next) → 5. any delegated
   `.agent-run/<mission-id>/RUN.json` receipt for decisions this mission
   dispatched to `implement`/`deliver`.

## How to verify (don't trust)

- Delegated decisions trace to a receipt: `QUEUE.md`'s "notes" column points
  at the `implement`/`deliver` `RUN.json` (or equivalent) that recorded it.
- **Test gates RED-on-mutation:** a gate that still passes when you flip the
  violation is decoration. Re-run the relevant proof with the invariant
  deliberately broken and confirm it fails.

## Run / resume / steer / stop

The mission is driven entirely through **`GOAL.md`**.

- **Resume / launch:** set `GOAL.md` → `STATUS: RUN`, open a Claude Code
  session **in the mission root** (`.agent-run/<mission-id>/`) at high
  effort, and paste the self-pacing loop below:

  ```
  /loop You are the conductor for the autopilot mission in this mission root.
  Read "$(provenant root)/skills/autopilot/references/operating-loop.md"
  IN FULL first, then GOAL.md (mission + STATUS gate + Active directives),
  STATE.md (your recover-after-compaction anchor), and QUEUE.md's head.
  Run ONE iteration of the 8-step loop (RECONCILE → READ → SELECT → DISPATCH →
  RECORD → PROPAGATE → REORG-if-due → STATE → WAKE/STOP), delegating every
  bounded wave to orchestrate and every decision/software change to
  implement/deliver rather than forking that lifecycle locally. If GOAL
  STATUS==STOP, write a clean handoff and HALT. Otherwise self-pace while
  work is active and use the validated idle-frontier PAUSED checkpoint when
  the frontier is dry. Before accepting PAUSED, run
  `python3 "$(provenant root)/skills/autopilot/scripts/validate_idle_pause.py" "STATE.md" --queue "QUEUE.md"`.
  A non-zero result means re-invoke one iteration; do not exit the driver.
  ```

  `/loop` with no interval **self-paces** (it schedules its own wake-ups).

  **Codex operator variant:** invoke the iteration from the validated
  external driver in `references/codex-operator.md`; it rejects premature
  pauses and stops self-waking after a valid dry-frontier checkpoint.
- **Steer:** add bullets under *Active directives* in `GOAL.md`. Empty =
  follow the default traversal order.
- **Answer a gate:** resolve the item in `STATE.md` Blockers (or add a
  directive / fill the relevant `GOAL.md` field). Your answer unblocks the
  dependent work.
- **Pause / stop:** a dry frontier creates a resumable `STATE.md` PAUSED
  idle checkpoint without changing human-owned `GOAL.md`. Set `GOAL.md` →
  `STATUS: STOP` only to close the mission; the conductor then refreshes
  the terminal handoff before halting.
- **Precondition:** the owning project is a Git repo. Linked-checkout spikes
  additionally need direct human authority and use only
  `<primary-root>/.worktrees/<name>` via the global `scripts/worktree`
  helper.

## Guardrails & durability

- **The build ceiling** — declared by the `BUILD_CEILING` knob in `GOAL.md`:
  the line between what the mission may build autonomously and what needs
  your sign-off. The conductor **never raises it itself**.
- **Delegated lifecycle** — model/effort routing, provider dispatch and the
  review ladder are `orchestrate`'s; decision records and software change
  are `implement`/`deliver`'s. This mission directory keeps only the
  pointers (`QUEUE.md` notes) plus whatever receipt those skills wrote here.

## Navigation map

```
.agent-run/<mission-id>/
├── README.md   ← you are here (the single human entry point)
├── GOAL.md     ← mission + Active directives + the STATUS: RUN/STOP switch
├── STATE.md    ← live heartbeat + Blockers worklist (resume from this alone)
├── QUEUE.md    ← durable work queue + item-lease ledger
├── HANDOFF.md  ← capstone synthesis + terminal pickup
└── RUN.json    ← present only once a delegated implement/deliver wave ran
```
