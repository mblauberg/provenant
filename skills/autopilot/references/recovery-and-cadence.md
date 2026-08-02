# Recovery and Cadence

How a long-running autonomous orchestration stays *alive and honest* across hours-to-weeks: surviving transient provider failures, resuming instead of restarting, telling stuck from slow, refusing to trust a worker's word, self-pacing its own wake-ups, and never halting until the goal file actually says stop.

This layer assumes the loop and the state contract described in the sibling references (`operating-loop.md`, `state-contract.md`) and the workflow runtime `orchestrate` provides. It is the discipline that makes all of them crash-safe and self-terminating.

> **Worked reference instance (aside):** these mechanics were hardened on a multi-week, ~100-iteration design-lab run with a "scaffold + local/mocked only" build ceiling. Every domain-specific value below is shown as a `{{CONFIG_KNOB}}` you fill at bootstrap; the mechanics themselves are domain-agnostic. The reference examples are illustrative anchors, never load-bearing rules.

> *Implemented by `templates/STATE.template.md` (the heartbeat/recover anchor), `templates/README.template.md` (the Claude `/loop` prompt and pause gate), and `references/codex-operator.md` (the Codex external driver).*

---

## 1. Re-entrancy is the foundation

Assume your context can be summarized, compacted, or wiped at any moment — between any two tool calls. The run must be fully reconstructable **from disk alone**. Two habits, both non-negotiable, make this true:

- **RECORD-BEFORE-LAUNCH.** Before dispatching *any* background job, flip its `QUEUE.md` row to `LEASED` with `{lease-owner, lease-expiry, what, expected-output-path}` filled. Only then dispatch. A crash in the window between launch and record orphans the run — the job runs to completion on disk but no surviving record points to it. Recording *first* guarantees the run→work-item link survives a wipe.
- **RECONCILE-FIRST.** Every iteration begins — before selecting any new work — by re-reading `QUEUE.md`'s `LEASED` rows and re-attaching each to its work-item:
  - completed → ingest its output (verbatim) and clear the lease (flip to `DONE`);
  - dead / expired → triage (resume or re-dispatch — see §2);
  - still-running → leave it, do not re-dispatch.

  This step re-attaches results to work *even if the prior iteration was compacted mid-flight*. It is the crash-safety primitive, not bookkeeping. Skipping it is how you lose a finished job's output or double-dispatch a running one.

The heartbeat file (`STATE`) is your single **recover-after-compaction anchor**: rewritten to current truth at the end of every iteration, re-read at the top of every iteration. If a fact is not on disk, it does not exist after the next compaction.

---

## 2. Transient-failure recovery (overload / rate / session limits)

Provider degradation — `{{TRANSIENT_FAILURE_SIGNALS}}` (e.g. an "overloaded" 5xx, rate-limit, session-cap reset) — is a **fact of long runs, not a reason to abort.** A multi-hour run *will* hit it. When a background job dies mid-run, follow this protocol in order:

1. **Preserve survivors first.** Before anything else, write every completed and partial sub-result to disk verbatim. A 26-agent workflow that dies at stage 4 still produced real, valuable stage-1..3 findings. Losing them to a panic-restart is the expensive mistake.
2. **Do no-API-safe deterministic work.** While the API is degraded, reconcile
   ledgers, re-verify counts and preserve findings. Remove or reorganise files
   only when the run manifest proves ownership and the existing authority allows
   that exact action; unknown or user-owned material stays untouched.
3. **Resume, do not restart.** Re-invoke the *same* workflow with its resume handle: `{{WORKFLOW_RUNNER}}({ scriptPath, resumeFromRunId: <prior-run-id> })`. Cached/completed sub-agents return **instantly from the journal**; only the failed-or-new sub-agents actually re-run. A resumed multi-stage pipeline re-runs only the killed stages, not the whole thing. (Codex substrate: same rule via the ledger — diff expected outputs vs on-disk files, re-dispatch only the missing items; `codex-operator.md`.)
4. **Lean on cross-family stages during a primary outage.** Stages that call a *different* model family (`{{CROSS_FAMILY_VERIFIER}}`, e.g. a non-primary CLI) are unaffected by the primary provider's overload and resume cleanly. They are your overload-immune capacity.

### Journal archaeology

The per-run journal (`wf_*.json` or your runner's equivalent) is **ground truth for how far a job got.** Read it to see which stages/agents completed vs died and to recover partial outputs. Never re-run blind, and never trust a *summary* about progress — open the journal.

### Hard-limit backoff: probe, don't busy-redispatch

Distinguish a momentary blip (a single overloaded response — just retry/resume) from a **hard limit** (a session/quota cap that holds for a known window). After a hard limit, on each wake dispatch exactly **one cheap probe** agent: *"reply with exactly: PROBE-OK"*.

- Probe returns → the limit cleared → resume/re-dispatch the voided or partial work.
- Probe fails → the limit still holds → re-schedule a **long** wake (`{{LONG_WAKE_SECONDS}}`, default ~3600s) and **end the turn**.

Busy-redispatching real work into a standing wall just burns iterations into the same failure for no result. The cheap probe + long backoff is the cure.

### Salvage triage: VOID vs PARTIAL

Classify every dead job before acting:

- **VOID** — nothing written to disk → re-dispatch **fresh**. "Resuming" a void produces nothing.
- **PARTIAL** — real files on disk → **KEEP and RESUME-to-complete; never rebuild from scratch.** Discarding a partial wastes work already paid for.

Record the verdict (VOID/PARTIAL) **and the exact salvaged paths** (in `{{SALVAGE_DIR}}` / the ledger) so the *next* iteration knows to resume rather than restart — the decision must survive a compaction too.

> Mis-triaging is costly in both directions. A skeleton of real files thrown away and re-dispatched is wasted compute; a "resume" pointed at a void produces silence.

---

## 3. Stuck vs still-running is an OS question

"Did the background job hang?" is answered by an **OS-level liveness check** (`{{PROCESS_CHECK}}` — inspect the actual process / PID), **not** by the task manager's status line. A job can read "running" while genuinely progressing slowly, or while truly hung — the status line cannot tell you which.

When a user asks "did the workflows get stuck?", **process-verify before answering.** Trusting the task list alone hides hangs.

---

## 4. Never trust a worker's self-reported verdict

A worker reporting `ok:true` / "tests pass" / "fixed" is a **claim, not evidence.** On anything high-stakes, re-run the verification yourself, or route it to an **independent `{{CROSS_FAMILY_VERIFIER}}`**, before recording the result as discharged. The independence boundary (the step that *built* the thing is never the step that *reports the verdict* on it) is structural — see `cross-family-review.md`.

Two concrete traps, both observed:

- **The diverged resume.** A resumed job launched with under-specified args (`args=null`) *self-selected a different task* than intended and reported `ok:true`. The instructed work never ran — "resume completed" did **not** mean "the work I wanted got done." → **Pin the target in `args` on resume, and verify the produced artifact, not the verdict.**
- **The placebo check.** A verification that *structurally cannot fail* — e.g. an "independent" oracle that imports the very arithmetic it is meant to check — reports green forever. → A check whose failure you have **never observed** is not yet evidence. Prove it can go RED (inject a fault; add a placebo-control) before you trust a green. A decorative gate found this way pierces any firm-stop below; route the fix through the owning `implement`/`diagnose` lifecycle.

---

## 5. Bounded retry — the convergence rule

A fix gets at most `{{MAX_RETRY_ATTEMPTS}}` attempts (default **2**). **If the 2nd fix still fails verification *with new gaps surfacing*, do NOT loop a 3rd time — ESCALATE** to the appropriate `{{ESCALATION_GATES}}` (user / domain-expert / judge-panel / spike / promotion-gate) and record the residual. Pre-declare the rule before fix #2 so the escalation is principled, not a surrender. A remediation that *diverged or never executed* (see §4) counts as a failed attempt, not a free retry.

Carve-out — **firm-stop vs genuine-placebo.** Be honest about which loop you are in:

- **DRAINING a finite, known list of buildable owed-items** → legitimate; finish them. The list does not grow as you work it.
- **RE-RUNNING an open-ended critique/harden loop that generates NEW divergent findings every pass** → stop; this is diminishing returns. The "findings" are the loop feeding itself.

And which finding you have:

- A self-review finding that is a **genuine correctness placebo** (a check that cannot fail; a rig that fakes a pass) **MUST be fixed** regardless of where you are in the firm-stop — it *pierces* the stop.
- A self-review finding that is **diminishing-returns coverage on an already-strong, independently-verified result** goes to an owed-list under the firm-stop.

---

## 6. Wake discipline and self-pacing

The loop is driven by a self-pacing scheduler (`{{WAKE_SCHEDULER}}`), not a busy-loop.

**Wake discipline:** when *all* selectable work is in-flight, do **not** busy-loop re-reading "everything in flight." Schedule the next wake and **end the turn.** Match the delay to the work:

- **Short** (minutes) when actively polling a specific running job;
- **Long** (`{{LONG_WAKE_SECONDS}}`, ~3600s) while backed off behind a hard limit;
  a genuinely dry frontier uses the validated idle pause and schedules no wake.

**Completion-notify is only a wake signal; before verification apply the `orchestrate` skill's `worker-liveness.md` quiescence rule.** The scheduled wake is a fallback. The harness re-invokes the orchestrator *on background-job completion*. So set a **long** fallback and rely on the notify: short polling fallbacks just waste iterations re-reading an unchanged in-flight table. Where a delay is genuinely optional, prefer one that stays within the runtime's cache window rather than re-paying cold-start cost.

**Steady-state transition** (once all reachable work appears complete):

1. reconcile any straggler,
2. verify `{{STOP_CONDITION}}` (the goal file's `STATUS`),
3. run one bounded re-enumeration pass; if still dry, write a `PAUSED`
   idle-frontier checkpoint with a resume trigger, release the lease and end the
   driver without another self-wake.

Do **not** fabricate busy-work, and do **not** re-run a pass that is already
current. A material directive, gate answer, external completion or explicit
restart resumes the mission. An idle steady state that loops is a bug.

---

## 7. The STOP-hook enforcement pattern

Wire a **Stop hook** that preserves the user-only mission terminal but allows a
durable idle pause. It re-invokes while selectable/in-flight work exists; a
`STATE: PAUSED` idle-frontier checkpoint ends the driver without claiming the
mission terminated. Only `{{STOP_CONDITION}}` closes the mission.

- The orchestrator runs an iteration and tries to end its turn.
- The Stop hook reads GOAL and STATE. If work exists, it re-invokes one
  iteration. Before accepting an idle pause it runs:

  ```sh
  python3 "${AGENTS_HOME:-$HOME/.agents}/skills/autopilot/scripts/validate_idle_pause.py" \
    "{{MISSION_DIR}}/STATE.md" \
    --queue "{{MISSION_DIR}}/QUEUE.md"
  ```

  A non-zero result re-invokes one iteration; a passing result exits the driver
  without closing the mission. If
  `STATUS == STOP`, it performs terminal handoff and exits the mission.
- The user steers by editing the goal file; the only terminal mission exit is
  `STATUS=STOP`.

**Substrate realisations of the same gate:** on Claude Code this is a literal
Stop hook plus `/loop` self-pacing. On a **Codex operator**, the validated
external driver in `codex-operator.md` runs one iteration per invocation,
re-invokes only while work is active or in flight, and releases its lease after
a valid idle-frontier pause. The gate semantics below apply to both.

**Why it matters:** the hook catches premature "we're done" declarations. **A second hook-fire is itself the signal that the orchestrator over-claimed completion** — repeatedly observed, the orchestrator deferred a finite list of *still-buildable* work to an owed-list and called the run done; the hook re-firing was the (correct) course-correction. Treat a re-fire as evidence you over-claimed, **not** as noise to suppress. Do not declare done while buildable work sits deferred.

**Clean handoff on STOP:** when `STATUS == STOP`, write a terminal handoff capturing live state — what's built, what remains and which `{{ESCALATION_GATES}}` own each residual, and verified counts — then HALT. **Re-verify counts independently** (count the artifacts on disk; reconcile `QUEUE.md` to zero leased rows) rather than trusting the last in-flight summary. `GOAL`, `STATE`, and `HANDOFF` must agree before the run is allowed to stop.

---

## Configuration knobs (fill at bootstrap)

| Knob | What it is | Default |
|---|---|---|
| `{{TRANSIENT_FAILURE_SIGNALS}}` | Provider errors that map to preserve-and-resume rather than abort | overload/5xx, rate-limit, session-cap |
| `{{WORKFLOW_RUNNER}}` | Background-run substrate with a resume path + readable per-run journal. Claude Code: `Workflow()` + `resumeFromRunId` + `wf_*` journal. Codex: subagent waves + `QUEUE.md` as journal, re-dispatching only missing items (`codex-operator.md`) | — |
| `{{WAKE_SCHEDULER}}` | Self-pacing scheduler. Claude Code: completion-notify primary + ScheduleWakeup fallback. Codex: the external loop driver's sleep | — |
| `{{PROCESS_CHECK}}` | OS-level liveness check (PID/process), independent of the task manager | — |
| `{{CROSS_FAMILY_VERIFIER}}` | A different-model-family verifier; provider-overload-immune | — |
| `{{LONG_WAKE_SECONDS}}` | Long active-backoff wake delay | ~3600 |
| `{{MAX_RETRY_ATTEMPTS}}` | Fix attempts before escalation (the convergence rule) | 2 |
| `{{MAX_CONCURRENT_JOBS}}` | Concurrency ceiling for background jobs | tuned |
| `{{STOP_CONDITION}}` | Single source of truth the Stop hook reads | goal-file `STATUS == STOP` |
| `{{SALVAGE_DIR}}` | Known on-disk location where partial sub-results are journaled verbatim | — |
| `{{ESCALATION_GATES}}` | Named targets a stalled/escalated item routes to | — |
| `{{BUILD_CEILING}}` | What the run may **not** actually execute | — |
| `{{LOCKED_CONSTRAINTS}}` | Settled inputs the run designs around, not relitigated | — |
