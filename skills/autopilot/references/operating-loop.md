# Operating Loop and Discipline

The engine of an autonomous, long-running, multi-agent mission. You are the
**conductor**: keep the durable state graph, delegate independent depth and
lifecycle execution to existing skills, adjudicate evidence and synthesise
verified outputs until a user writes STOP.

This layer is domain-agnostic. Everything domain-specific is a **named config knob** you fill once at bootstrap. The same loop drives a literature review, a codebase migration, a security audit, an architecture design, or a market analysis with zero changes to this discipline.

> Implemented by `templates/GOAL.template.md` + `templates/STATE.template.md`
> (the on-disk state the conductor reads in full at the start of every
> iteration) and `references/state-contract.md` (the file-set contract).
> This doc is the spec of record for the loop itself.

---

## Bootstrap config knobs

Fill these once, in `GOAL.md`, before the first iteration. They are settled inputs, not decisions to reopen mid-run.

| Knob | What it pins |
|---|---|
| `{{DOMAIN}}` / `{{MISSION}}` | What this run produces and its north star (e.g. "exhaustively design and scaffold X", "synthesize the literature on Y", "audit Z for class-W vulnerabilities"). |
| `{{LOCKED_CONSTRAINTS}}` | A do-not-re-litigate list the run designs **around**, not toward. Settled inputs. |
| `{{ESCALATION_GATES}}` | Which work routes to which gate when the run cannot finish it autonomously. The taxonomy is generic (see §4); the specific gates are knobs. |
| `{{BUILD_CEILING}}` | The line between what the run may build/produce autonomously and what requires explicit user authorization (e.g. "scaffold + local/mocked only; no real infra, no irreversible side effects"). |
| `{{RUNAWAY_CAPS}}` | The tunable ceilings of §0a (concurrency, forks, depth, per-unit budget). |

Model/effort routing and hard-gate review panels are **not** knobs of this
skill — every dispatched wave routes through `orchestrate`, which owns
provider routing, topology and the review ladder. Decisions worth a durable
record delegate to the owning run's `implement`/`deliver` lifecycle (see
`state-contract.md`). Autonomy is **graduated by blast radius**: aggressive
on low-risk surfaces, conservative on the irreversible core.

---

## §0. Prime directive

Produce the most thoroughly-reasoned output a **team-equivalent** could, by fanning agents over every work unit, forking at hard branch points, judging adversarially, and going **deeper and broader** until the user writes STOP in the goal file.

- **Spend is proportional to risk and information gain.** Correctness, coverage
  and traceability outrank token thrift, but repeated low-yield fan-out stops at
  the convergence caps (§0a).
- **Never self-close the mission.** An empty queue triggers one bounded
  re-enumeration pass. If the frontier remains dry, write an idle checkpoint and
  pause the operator without another self-wake. Only the user STOP gate
  terminates the mission (see §6).

---

## §0a. Runaway caps

Bound the open-endedness so a long run never explodes. Enforce `{{RUNAWAY_CAPS}}`:

| Cap | Default | Rule |
|---|---|---|
| Max concurrent background jobs | ~4 | More queue as "next up"; launch as slots free. |
| Max active parallel-deep-tracks (forks) | ~3 | Additional fork-worthy units wait "warm". |
| Fork depth | ≤ 2 | One sub-fork; deeper requires a user note. |
| Per-unit budget before escalation | ~3 jobs | If a **single work unit consumes more than ~3 jobs without converging, stop fanning and escalate to the user.** |
| Per-iteration spend checkpoint | every state rewrite | Log jobs launched this iteration and cumulative spend. |

**No silent runaway.** If you hit a cap, say so in the state file. Never quietly drop work. **Caps are ceilings, not targets** — within them, prefer depth.

---

## §1. Protect the conductor's context

Manage your context window as a scarce resource. Keep durable pointers and
retain the load-bearing synthesis/adjudication role.

- **Offload state to the filesystem.** Your memory is `GOAL.md`, `STATE.md`
  and `QUEUE.md` under `.agent-run/runs/<mission-run-dir>/` (see `state-contract.md`) —
  not your context. After every meaningful step, write to disk and keep only
  a one-line pointer in your head. Offload any tool result over ~20k tokens
  to a file; keep a path plus a short preview.
- **Delegate independent depth AND lifecycle execution.** Fan work out to
  `orchestrate` waves; hand software change and decision records to
  `implement`/`deliver`. Read structured outputs, then perform the
  load-bearing synthesis/adjudication yourself or assign one named stage
  owner. Delegation does not outsource accountability.
- **Preserve provenance, then curate.** Keep raw returns or hashes when audit
  policy requires them. Durable reasoning may be composed from verified
  evidence with citations to source artifacts; never paste an unreviewed worker
  return as project truth. Record thin/failed legs and re-dispatch or escalate.
- **Stay re-entrant / crash-safe.** Assume your context may be summarized or compacted at any time. Everything needed to resume must live in the **state file**; never rely on remembering something you didn't write down. **Re-read the state file at the start of every loop** — it is the recover-after-compaction anchor.
- **One active orchestrator lease.** Enforce acquire/renew/transfer/release with
  `skills/orchestrate/scripts/lease.py`; STATE records chair/session, generation and
  heartbeat. A Claude/Codex peer owns bounded stages, never a second loop
  driver. Takeover requires a persisted handoff and generation increment.

---

## §2. The iteration loop

```
RESUMABLE MISSION LOOP until GOAL STATUS == STOP (a dry frontier PAUSES it):

  0. RECONCILE   read QUEUE.md's LEASED rows. For each:
                   completed     -> ingest output (go to RECORD), then clear the lease
                   dead/expired  -> re-dispatch, or mark BLOCKED
                 (this is the crash-safety step: it re-attaches results even
                  after a mid-flight compaction wiped them from your context)

  1. READ        GOAL.md (active directives + STOP status), STATE.md, QUEUE.md head

  2. SELECT      next unblocked PENDING unit. Respect depends-on + the §0a caps.
                 Prefer the highest-tier one-way-doors first.

  3. DISPATCH    BEFORE launching, flip the QUEUE.md row to LEASED with
                   { lease-owner, lease-expiry, what, expected output path }
                 filled. THEN launch the workflow / fan out the agents.
                 Run INDEPENDENT units concurrently up to caps — do NOT serialize.

  4. RECORD      preserve returned artifacts/receipts; delegate any durable
                 decision to implement/deliver (its receipt lands under the
                 same .agent-run/runs/<mission-run-dir>/); mark the QUEUE.md row
                 DONE/BLOCKED/DEFERRED; clear the lease.

  5. PROPAGATE   add any newly-surfaced work to QUEUE.md, with its deps.

  6. REORG       if a cadence trigger fired, tidy STATE.md's hot window (see
                 recovery-and-cadence.md).

  7. STATE       rewrite STATE.md: what happened this iteration, what is
                 leased, what is next, open forks, blockers, spend checkpoint.

  8. WAKE/STOP   if STOP -> write a clean HANDOFF.md + HALT.
                 else if selectable PENDING work remains -> continue NOW.
                 else if work is LEASED -> schedule a matched fallback wake.
                 else -> run one bounded re-enumeration pass; if still dry,
                      write STATE PAUSED + idle checkpoint + resume trigger,
                      release the lease, and END without another self-wake.
```

### Record-before-launch is the load-bearing invariant

Flipping the QUEUE.md row to LEASED **before** you dispatch means a crash or compaction in the window between *dispatch* and *record* never orphans a running job. RECONCILE (step 0) re-attaches it on the next wake. This single ordering rule is what makes the whole loop survivable. If you only learn one mechanic, learn this one.

### Wake discipline

The loop is **self-paced**. When all selectable work is in-flight:

- **Schedule your own next wake** and exit the turn — do not re-read "everything in flight" repeatedly and burn iterations.
- **Short delay** when polling a running job that should finish soon; **long
  delay** only for active backoff. A genuinely dry frontier pauses without a wake.
- The harness also re-invokes you on background-job completion, so the **scheduled wake is a fallback, not the primary signal**. Completion-notify is primary.

---

## §3. Delegation: when to use what

| Tool | Use for | Notes |
|---|---|---|
| **Workflow** | Anything multi-stage or fan-out: deterministic orchestration of many agents with pipelines, parallelism, judge panels, loops. | Runs in the background, notifies on completion. **Run several concurrently** across independent units. Prefer this for breadth-then-depth on a single unit. |
| **Single agent** | One read/research/design delegation. | **Batch independent agent calls in one message** so they run concurrently. One call = one serialization point; never serialize independent reads. |
| **Cross-family verifier** | Independent second opinions, adversarial verification, alternative implementations. | A different model family, routed through `orchestrate`. **Cross-family agreement materially raises confidence on one-way-doors.** See `cross-family-review.md`. |

Fan-out topology, workflow archetypes and provider routing are `orchestrate`'s
job — dispatch every bounded wave through it and record the returned
route/result receipt in `QUEUE.md` rather than restating that policy here.

**Rule of thumb:** fan out for **breadth** (many independent angles); go **deep/sequential** once a direction leads. Most units want both: fan out → judge → deepen the winner.

### Model + effort per call

Owned by `orchestrate` (single-source; do not fork a lab-local matrix). Every
`agent()`/wave call dispatched from this mission inherits its routing;
record the concrete models/effort it resolved to in `QUEUE.md`'s
`what`/notes column, not a private policy here.

### Topology: one accountable chair

- Subagents report compressed results to the stage owner. In paired-primary
  mode, Claude and Codex exchange namespaced artifacts under one chair; they do
  not run competing orchestration loops.
- Subagents are **intelligent filters**: they burn tokens exploring, and you see only the result. This is precisely what lets the run go long without context-rot.
- **Split work along context boundaries** (genuinely independent sub-questions), **not along roles.** Splitting work that shares heavy context across two agents forces a lossy handoff and defeats the purpose.

---

## §4. Bounded retry and the convergence rule

Maker-checker and fix loops are **bounded to ~3 iterations**, require **measurable improvement** each pass, and **escalate to the user on stall**. Never loop indefinitely.

Concretely:

- **1st fix:** legitimate, full attempt.
- **2nd fix:** if its **independent** re-verify **still fails with NEW gaps → STOP fixing.** Escalate the item as a clearly-marked escalation-gated residual. **Do not launch a 3rd build.** The run finishes with that residual escalated, not as an infinite harden loop.
- **Pre-declare this rule when you launch the 2nd fix**, so the convergence point is unambiguous in the record.
- A remediation that **diverged or never executed** (e.g. resumed with null args and self-picked a different target) counts as a **failed attempt**, not a free retry.

The escalation taxonomy this routes into is generic — **user tie-break / expert sign-off / judge-panel / evidence-spike / apply-step / promotion-review** — with the specific gates supplied by `{{ESCALATION_GATES}}`. See `recovery-and-cadence.md` §4–§5 for the never-trust-self-report and bounded-retry mechanics; decision records themselves are delegated to the owning `implement`/`deliver` lifecycle (see `state-contract.md`).

---

## §5. Anti-patterns

Do not:

- **Do deep analysis in your own context.** Delegate it and record the return.
- **Serialize independent work.** Fan out; batch independent agent calls in one message.
- **Pick a one-way-door from one agent's opinion.** Fork it, judge it adversarially, cross-family-verify it.
- **Let the tree sprawl.** Reorg on cadence and run the integrity sweep.
- **Inherit prior scoping numbers as decisions.** They are priors to test, not settled outputs.
- **Claim done without verification** — or while finite known buildable work remains. Re-enumerate once to confirm the frontier before pausing.
- **Block the whole run on one slow fork.** Keep it warm with a trigger and proceed elsewhere.
- **Author content to look busy.** Once genuinely dry, reconcile and pause; do not fabricate passes or schedule idle churn.
- **Trust a worker agent's self-reported verdict.** Read the independent verifier's result, never the worker's self-claim (see `cross-family-review.md`).

---

> **Worked reference instance (illustration only — not load-bearing):** these patterns were hardened in a ~100-iteration regulated-fintech design lab. Its locked constraints, hard gates, build ceiling and cross-family verifier are examples of knobs to fill at bootstrap, not defaults for another domain.
