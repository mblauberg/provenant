# STATE — conductor heartbeat
Updated: <YYYY-MM-DDTHH:MM:SSZ>
<!--
  CONDUCTOR-OWNED. Rewritten every iteration. This is the durable memory that
  SURVIVES context compaction — anyone (a human, or a fresh conductor
  session) must be able to resume the mission from THIS FILE plus QUEUE.md
  ALONE. If it isn't written here, it does not exist. Re-read this at the
  start of every loop (see references/operating-loop.md).

  Two parts: (1) a small mutable HEARTBEAT header at the top that you
  OVERWRITE each iteration with the current truth; (2) a newest-first HOT
  NOTE WINDOW below it. Keep five notes here; older notes may be dropped once
  their conclusions are captured in HANDOFF.md.
-->

## Heartbeat (OVERWRITE this block every iteration)

- **Run status:** RUNNING | PAUSED — reason: idle-frontier | STOPPED — one line on the live posture.
- **Conductor lease:** <chair family/session · generation · acquired-at · heartbeat-at; use release-on-driver-exit for PAUSED>
  <!-- exactly one active loop driver; takeover increments generation after a persisted handoff -->
- **Iteration:** <N>
- **This iteration:** <one line: what just happened — the headline result>
- **In flight:** <count of QUEUE.md LEASED rows, or a one-line summary; use (none) for PAUSED>
  <!-- QUEUE.md is authoritative for the leased rows themselves; this is a pointer, not a duplicate table -->
- **Next up:** <the next selectable unit(s) and why; use (none — dry after bounded re-enumeration) for PAUSED>
- **Open forks:** <Fxxx: branch question · status · kill-switch + deadline> (+ the warm waitlist)
- **Blockers / escalations:** <gated items ({{ESCALATION_GATES}} class), cap hits, anything needing the human>
- **Spend checkpoint (§0a):** <jobs launched this iter · cumulative · any unit over
  the ~3-run budget>
- **Resume protocol:** <for PAUSED, use exactly `restart-on:` plus a comma-separated
  subset of `human-directive`, `gate-answer`, `external-completion`,
  `material-change`, `explicit-restart`; for RUNNING, record the exact recovery
  steps and retained partial paths. This is what makes the mission crash-safe.>

> **STOP handling:** when `GOAL.md` STATUS flips to STOP, the final heartbeat must
> state the terminal truth (build-ceiling reached, the escalation-gated remainder,
> where the next agent/human starts) and `HANDOFF.md` must agree. GOAL + STATE +
> HANDOFF agreeing on the terminal truth is the finish gate.

---

## Hot note window (newest-first — maximum five)

<!--
  Convention for each entry (keep them dense; this is a ledger, not an essay):
    - **Note (iter<N>): <headline — what landed / what was caught>.** <1–4 sentences
      of detail: the result, the evidence, any decision/fork spawned, what was
      dispatched next.>
  Flag corrections explicitly: if a later iteration finds an earlier note
  over-claimed (e.g. "done" while buildable work remained, or a gate was a
  placebo), write a NEW note that says "⚠️ COURSE-CORRECT — iter<M>'s X was
  over-claimed" and leave the old note in place. Use ⚠️ for catches/pierces
  and ⭐ for headline wins so the log skims.
-->

- **Note (iter0):** Mission scaffolded from templates by
  `scripts/bootstrap-autopilot.py`. `GOAL.md` CONFIG KNOBS block
  {{filled?}}. `QUEUE.md` empty. STATUS: RUN. Next: seed `QUEUE.md` from the
  first work-enumeration pass, then dispatch via `orchestrate`.
