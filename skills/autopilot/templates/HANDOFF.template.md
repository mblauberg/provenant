# HANDOFF — human-handoff capstone  ·  <LIVE | TERMINAL> state (iteration <N>)
<!--
  CONDUCTOR-OWNED, regenerated on material change and refreshed at STOP. This
  is the CAPSTONE deliverable: it takes the mission's output from
  "adversarially validated on delegated skills' terms" to "handed to a human
  / next agent to carry forward." It SYNTHESIZES existing mission artifacts —
  it does NOT introduce new decisions (a handoff that decides is a bug).
  Every claim traces to a source file (relative paths from the mission root,
  or a delegated .agent-run/runs/<mission-run-dir>/RUN.json receipt).

  Until the mission produces artifacts this is a STUB. Closing the mission
  is a finish-blocker unless GOAL + STATE + HANDOFF all agree on the
  terminal truth.
-->

> **Purpose.** Capstone for taking `{{MISSION}}` from *adversarially
> validated* to *carried forward*. Synthesis only — no new decisions.
>
> **Run status:** <LIVE refresh | ⛔ FINISHED (human-authorized STOP)>. The
> mission sits at its `{{BUILD_CEILING}}` build-ceiling. <One line: what is
> complete vs what remains.>
>
> **Authoritative sources:** `GOAL.md` (mission + `{{LOCKED_CONSTRAINTS}}`) ·
> `STATE.md` (heartbeat, iteration <N>) · `QUEUE.md` (work items + count
> summary) · any delegated `.agent-run/runs/<mission-run-dir>/RUN.json` receipt from
> `implement`/`deliver` waves this mission dispatched.

---

## 0. TERMINAL PICKUP — the next agent / human starts HERE

<One paragraph: the mission's terminal state in plain terms — what is
build-complete, what is verified, and that the genuine work frontier is
exhausted (cite the re-enumeration passes). State plainly that the SOLE
remainder is the escalation-gated list below, and that NONE of it is
conductor-dispatchable — each item is owned by a human, an expert authority,
a judge panel, a spike, an apply-time act, or a promotion review.>

**START with #<k> — <the single highest-priority remainder item and why it's first>.**

### The escalation-gated remainder (the 6 FIXED classes; items are `{{ESCALATION_GATES}}`)

*(Each item: `[class] short-id` — what it is · what it blocks · where its
receipt/spec lives.)*

- **[promotion]** — artifacts that may only be promoted past a review gate.
- **[expert]** — items needing a named external sign-off authority.
- **[human]** — one-way-door picks a person must own (provisional default recorded).
- **[judge/panel]** — calls needing an adversarial judge panel + cross-family pass.
- **[spike]** — questions only a runnable prototype answers.
- **[apply]** — acts that take effect only at real apply-time (deferred gates).

## 1. Executive summary

- **What this is.** <one paragraph: the deliverable, the locked posture
  (`{{LOCKED_CONSTRAINTS}}`), the centre of gravity (`{{MISSION}}`).>
- **What the mission produced.** <QUEUE.md count summary · delegated
  implement/deliver receipts · cross-family reviews run · open forks · the
  headline build result, with evidence.>
- **The headline finding.** <the single most important result + its evidence.>

## 2. What was decided / built

<Organized by dependency tier or work area. One bullet per load-bearing
item: `QUEUE.md` id → the call → the one-line why → the delegated receipt
path (`.agent-run/runs/<mission-run-dir>/RUN.json` or equivalent) if any.>

## 3. The open forks = the spike/decision backlog

<Each open fork: `Fxxx` — branch question · status (open/converging/warm) ·
the kill-switch + convergence deadline · what evidence resolves it.>

## 4. Recommended build / promotion sequence (the critical path)

<The ordered path a builder/human takes to carry this forward: what to
promote first, what each step unblocks, where the delegated gates sit on
the path.>

## 5. What the mission did NOT do (the `{{BUILD_CEILING}}` ceiling)

<The explicit ceiling the artifacts matured TO but did not cross. State it
plainly so no one mistakes a scaffold for a deployed system.>

## 6. Housekeeping

<Reorgs done, any known-stale references being tracked, `QUEUE.md`'s count
summary as verified (pending/leased/done/blocked/deferred all reconciled to
zero leased before STOP).>
