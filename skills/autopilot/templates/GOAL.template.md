# GOAL — the north star + the RUN/STOP gate
<!--
  HUMAN-OWNED FILE. The conductor reads this at the START of every iteration
  and obeys it. The conductor must NEVER author content into this file — it
  is the one place the human steers and stops the mission. The ONLY clean
  exit is a human setting STATUS: STOP below.

  This template is domain-agnostic. Every domain fact lives in the CONFIG
  KNOBS block — fill it ONCE, then run scripts/bootstrap-autopilot.py to
  validate. GOAL.md is intentionally the only file you hand-edit.
-->

```config-knobs
# ============================================================================
# CONFIG KNOBS — fill these ONCE, then re-run scripts/bootstrap-autopilot.py
# to validate. This fenced block is the SINGLE domain-injection point for the
# whole mission. A thin fill produces thin output — write a substantive
# MISSION + constraint set. Each knob: replace the {{...}} value; keep the
# KEY = on the left.
# ============================================================================

DOMAIN            = {{DOMAIN}}
# ^ one line naming the field/system this mission operates in.

MISSION           = {{MISSION}}
# ^ the open-ended north star + definition-of-good: what the mission
#   PRODUCES, run until STOP. Be concrete and substantive.

LOCKED_CONSTRAINTS = {{LOCKED_CONSTRAINTS}}
# ^ the do-not-relitigate set the mission designs AROUND — settled inputs,
#   NOT decisions to reopen.

BUILD_CEILING     = {{BUILD_CEILING}}
# ^ the explicit line between what the mission may build AUTONOMOUSLY and
#   what requires human authorisation / is owed / escalated.

ESCALATION_GATES  = {{ESCALATION_GATES}}
# ^ the concrete instances, for THIS mission, of each of the 6 GENERIC gate
#   classes (the taxonomy is fixed; the members are the knob): human / expert
#   / judge-panel / spike / apply / promotion. List named items per class so
#   the conductor designs AROUND them instead of stalling.

RUNAWAY_CAPS      = {{RUNAWAY_CAPS}}
# ^ the ceilings that keep a multi-week mission from exploding (see
#   references/operating-loop.md §0a). Defaults: max ~4 concurrent jobs ·
#   max ~3 active forks · fork-depth ≤2 · ~3 runs per unit before escalating
#   · bounded-retry ≤2 · long-wake ~3600s. Ceilings, not targets.
# ============================================================================
```

## Mission (open-ended — run until STOP)

{{MISSION}}

The framing is **never declare "done" without human STOP.** An empty queue
triggers one bounded re-enumeration pass. If the frontier remains dry, the
conductor writes an idle checkpoint and pauses dispatch until a material
resume trigger. That pause is resumable; only `STATUS: STOP` in this file
closes the mission.

## Traversal order (default — "Active directives" below override it)

1. **Foundational one-way-doors first.** Drive the highest-blast-radius,
   hardest-to-reverse decisions early via `orchestrate` waves rather than a
   single-agent pick. Flag the `{{ESCALATION_GATES}}` ones and design AROUND
   them — never stall the whole mission on a gated item.
2. **Descend the dependency tiers**, respecting `depends-on` in `QUEUE.md`;
   run INDEPENDENT units concurrently within the `{{RUNAWAY_CAPS}}` caps.
3. As work settles, **build/decide** up to `{{BUILD_CEILING}}` by delegating
   to `implement`/`deliver`, whose receipts land in this mission directory.
4. **When the queue empties, re-enumerate once.** If no real work appears,
   persist an idle checkpoint and pause; never invent depth merely to stay
   busy.

## Locked constraints (do NOT relitigate)

{{LOCKED_CONSTRAINTS}}

> Design around these; do not reopen them. They are settled inputs, not
> decisions.

## Escalation-gated items (design around — do not stall the mission)

{{ESCALATION_GATES}}

> Each of these is owned by a human, an expert authority, a judge panel, a
> spike, an apply-time act, or a promotion review — not by the conductor.
> Build to the intent, keep the flagged optionality, record the open
> question, mark the gate in `STATE.md` "Blockers", and proceed on
> everything else.

## Active directives (human-editable steering)

> Add bullets here to focus the mission on something specific. Empty =
> follow the traversal order above. This block overrides the default
> traversal.



## STATUS gate

To stop: change `STATUS:` below to `STOP` (and interrupt the loop if
running). The conductor checks this **every iteration** and writes a clean
handoff in `STATE.md` + refreshes `HANDOFF.md` before halting.

> **Flipping to STOP is a finish-blocker unless GOAL + STATE + HANDOFF all
> agree on the terminal truth.** A STOP written while the capstone is stale
> is a bug. When you flip it, write an inline audit note (who / why / when)
> and update PREV.

STATUS: RUN
<!-- audit note: who/why/when this last flipped. RUN at scaffold. -->
PREV: (none)
