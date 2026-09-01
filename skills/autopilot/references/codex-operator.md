# Operating autopilot from Codex

How a **Codex** session runs the full autopilot loop as the conductor. The
state contract, 8-step loop, and escalation taxonomy are **identical** to the
Claude Code substrate — only the dispatch mechanics and the re-invocation
wiring differ. Everything in `operating-loop.md`, `state-contract.md`, and
`recovery-and-cadence.md` applies unchanged.

**Do not pass Claude workflow JavaScript to Codex.** Codex has no Claude
`Workflow()` runtime, `/loop`, ScheduleWakeup or Stop hook. An eligible GPT-5.6
lead at `ultra` can proactively coordinate native subagents and therefore run
the same portable workflow graph adaptively; lower efforts use explicit waves.
The external driver still owns cross-session re-invocation and the STOP gate.

## Substrate mapping

| Claude Code primitive | Codex realisation |
|---|---|
| `Workflow()` background run | Ultra/native multi-agent stage graph, or one explicit wave per stage when Ultra is unavailable |
| `pipeline(items, s1, s2…)` | Per-item sequential subagent chains inside a wave; reduce in the main thread |
| `parallel(thunks)` | One wave of subagents; wait for all; reduce |
| `agent(prompt, {schema})` | Subagent prompt ending with an explicit return contract ("return ONLY this JSON shape"); validate on ingest, re-dispatch on malformed (bounded by the ~2-attempt convergence rule, then escalate) |
| `resumeFromRunId` (journal cache) | **`QUEUE.md` IS the journal**: on RECONCILE, diff expected outputs vs on-disk files and re-dispatch **only the missing/failed (expired-LEASED) items**, never the whole wave |
| `/loop` + ScheduleWakeup | The external loop driver below; wake pacing = the driver's sleep |
| Stop hook | The driver's `STATUS == STOP` check — same gate, same goal file |
| `/workflows` progress view | `STATE.md`'s heartbeat + `QUEUE.md`'s LEASED rows |

At `ultra`, Codex may delegate proactively. At other efforts, make every fan-out
concrete: "Spawn one subagent per work-unit below. Wait for all. Return the
reduce table only." Even under Ultra, preserve the work-unit, write-scope,
artifact and reduction contracts; proactive delegation does not widen authority.

## The external loop driver

The Codex equivalent of Stop-hook-plus-`/loop`. Run it from the mission root
(`.agent-run/<mission-id>/`); steer and stop exactly as on Claude Code — edit
`GOAL.md` directives, set `STATUS: STOP`.

```sh
#!/bin/sh
# autopilot-driver.sh — re-invoke active iterations; exit on valid PAUSED or user STOP.
MISSION_DIR="${1:?usage: autopilot-driver.sh <MISSION_DIR>}"
LEASE_TOOL="$(provenant root)/skills/orchestrate/scripts/lease.py"
PAUSE_VALIDATOR="$(provenant root)/skills/autopilot/scripts/validate_idle_pause.py"
LEASE="$MISSION_DIR/LEASE.json"
HOLDER="codex-driver-$$"
python3 "$LEASE_TOOL" acquire "$LEASE" --holder "$HOLDER" --ttl "${MISSION_LEASE_SECONDS:-900}" || exit 1
trap 'python3 "$LEASE_TOOL" release "$LEASE" --holder "$HOLDER" >/dev/null 2>&1 || true' EXIT INT TERM HUP
PROMPT="You are the conductor for the autopilot mission at $MISSION_DIR.
Read "$(provenant root)/skills/autopilot/references/operating-loop.md"
and recovery-and-cadence.md IN FULL first — they are your constitution.
Then read GOAL.md, STATE.md, and QUEUE.md's head. Run ONE iteration of the
8-step loop (RECONCILE → READ → SELECT → DISPATCH → RECORD → PROPAGATE →
REORG-if-due → STATE → WAKE/STOP), obeying: protect conductor context,
record-before-launch, never self-close the mission, delegate lifecycle
execution to implement/deliver/orchestrate rather than forking it locally.
You are on the Codex substrate: dispatch via native parallel subagent waves
per references/codex-operator.md, NOT Claude workflow JavaScript. If GOAL
STATUS==STOP, write a clean handoff and end. Otherwise end the turn when all
selectable work is leased or recorded."
while ! grep -q '^STATUS: *STOP' "$MISSION_DIR/GOAL.md"; do
  python3 "$LEASE_TOOL" renew "$LEASE" --holder "$HOLDER" --ttl "${MISSION_LEASE_SECONDS:-900}" || exit 1
  codex exec --cd "$MISSION_DIR" "$PROMPT" & iteration_pid=$!
  (
    while kill -0 "$iteration_pid" 2>/dev/null; do
      sleep "${MISSION_LEASE_HEARTBEAT_SECONDS:-240}"
      python3 "$LEASE_TOOL" renew "$LEASE" --holder "$HOLDER" --ttl "${MISSION_LEASE_SECONDS:-900}" >/dev/null || {
        kill "$iteration_pid" 2>/dev/null || true
        exit 1
      }
    done
  ) & heartbeat_pid=$!
  wait "$iteration_pid" || sleep 300                  # transient-failure backoff
  kill "$heartbeat_pid" 2>/dev/null || true
  wait "$heartbeat_pid" 2>/dev/null || true
  if grep -Eq '(^- \*\*Run status:\*\* .*PAUSED|^PAUSED([[:space:]]|$))' "$MISSION_DIR/STATE.md"; then
    if python3 "$PAUSE_VALIDATOR" "$MISSION_DIR/STATE.md" \
        --queue "$MISSION_DIR/QUEUE.md"; then
      python3 "$LEASE_TOOL" release "$LEASE" --holder "$HOLDER" >/dev/null || exit 1
      trap - EXIT INT TERM HUP
      break                                           # valid idle pause; mission stays RUN
    fi                                                # invalid pause cannot stop re-invocation
  fi
  sleep "${MISSION_WAKE_SECONDS:-60}"                  # active/in-flight pacing only
done
```

Driver notes:

- **One iteration per invocation.** The inner session must end its turn (not self-loop); the driver
  owns re-invocation, exactly as the Stop hook does on Claude Code. A driver re-fire after a "done"
  claim is the same over-claim signal as a Stop-hook re-fire (`recovery-and-cadence.md` §7).
- **Idle is a pause, not completion.** After bounded re-enumeration finds no real
  work, persist STATE `PAUSED — reason: idle-frontier`, an empty in-flight/next
  frontier, `release-on-driver-exit`, and a structured external resume trigger:
  `restart-on:` followed only by `human-directive`, `gate-answer`,
  `external-completion`, `material-change`, or `explicit-restart`.
  The validator also proves `QUEUE.md` has no `LEASED` row, no `PENDING`
  selectable work, and the resume trigger uses that enum; it rejects
  premature pauses before the driver releases its lease and exits. A
  material change or explicit restart launches it again; only user
  `STATUS: STOP` closes the mission.
- **Crash-safety is unchanged**: record-before-launch + RECONCILE make a killed `codex exec`
  recoverable — the next invocation re-attaches from `QUEUE.md`. Session continuity is a
  nice-to-have (`codex exec resume`), never a dependency.
- Use `MISSION_WAKE_SECONDS` only while work is active or in flight. A dry frontier
  exits through the durable PAUSED checkpoint instead of a long idle loop.
- **Canonical STOP line**: the driver greps `^STATUS: *STOP` — the goal file must carry exactly
  `STATUS: STOP` at line start (that casing). Anything else does not halt the run.
- **Supervise the driver.** The driver is the one process whose death stops re-invocation (state
  and queue survive; nothing re-launches iterations). Run it under `tmux`/`nohup`/a service
  supervisor; if it dies, just restart it — RECONCILE re-attaches everything.
- **Model/effort per wave**: routed through `orchestrate`, not this skill. Discover what the local
  Codex install exposes at runtime (`codex exec --help`, profiles in `~/.codex/config.toml`) when
  `orchestrate` dispatches a Codex-substrate wave; never guess model names from memory.

## Cross-family, family-relative

The independence rule is "a family **≠ the operator's**", never a fixed vendor list. With Codex
(OpenAI) operating: the load-bearing verifier is **claude** (Anthropic).
Gemini (Google) is an optional distinct-family lane. A `codex exec` "verifier" from a
Codex operator is a same-family self-review: it **cannot certify** a hard gate — the wrapper
fail-closes it. All capture/independence rules of `cross-family-review.md` (raw-verdict-to-disk,
never-trust-self-report, build≠verdict-reporter) apply unchanged.

Model/effort routing is likewise operator-relative and owned by `orchestrate`,
substituting the Codex model lineup for the flagship/workhorse instance
(flagship reasoning tier for judgement/design/synthesis + all adversarial
judges; a cheaper tier only for mechanical breadth under a flagship-authored
contract). This skill does not keep its own copy of that policy.

## Codex-operator pitfalls

- **Silent under-fan-out.** Codex may keep work in the main thread. Delegate
  genuinely independent depth and record any reason not to; keep synthesis and
  adjudication with the accountable stage owner.
- **Wave = barrier.** Native waves join before reduce; don't hold a mega-wave open across the whole
  queue. Dispatch per work-unit batches sized to `{{RUNAWAY_CAPS}}`.
- **No background completion-notify.** Unlike Claude Code, nothing re-invokes mid-turn on job
  completion; RECONCILE at the top of each driver iteration is the only re-attach point. Keep
  iterations short so the ledger stays fresh.
