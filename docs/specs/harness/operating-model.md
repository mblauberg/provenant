# Spec: harness operating model for routing, cache, quota and telemetry

Status: **draft** for user acceptance (grilled 2026-10-21). GitHub owns
delivery state; this file owns the durable requirements. It grants no
authority.

## Outcome

The user scopes outcomes and steers; the harness does the rest without being
reminded. Concretely:

- the chair picks shape, tier, model and effort from policy it is given, not
  from memory it has to be prompted to recall;
- work spreads across every provider and every account the user holds,
  switching automatically as quota runs out, on both hosts;
- tokens go to useful work rather than cold-cache rewrites, idle coordinators
  and oversized tool output;
- every run leaves joined, deduplicated telemetry, so later retrospection
  works from data rather than transcripts.

## Evidence baseline (60 days to 2026-10-21)

Study artefacts live outside the repository; the figures below are the ones
acceptance is measured against.

| Signal | Baseline | Source |
|---|---|---|
| Subagent share of Claude tokens | 77% (42.9B of 55.5B) | transcript audit, deduped by `message.id` |
| Sealle Opus subagent tokens | 25.9B; 89% from an explicit `model: "opus"` on the spawn | spawn metadata |
| Opus subagent tokens by role | implementer 44%, coordinator 22%, reviewer 13%, researcher 10%, lander 4% | role classifier, 150-file hand check |
| Cache writes that are cold rewrites after a gap over 5 min | 42% of Sealle cache writes (about 710M tokens) | gap audit |
| Gap causes | foreground Bash wait or poll 43%, other long Bash 41%, end of turn 15% | gap audit |
| Compaction cost | about 58M tokens (minor) | compact boundaries |
| Bash share of tool-result characters | 85% | transcript audit |
| Subagent first-call context | p50 36k (main 11k) | transcript audit |
| User routing reminders | about 460 (est.) | user-message classifier |
| User autonomy nudges | about 600 (est.) | user-message classifier |
| Manual `/compact continue` re-primes | 349 | user messages |
| User `/usage` checks | 146 | user messages |
| Free OpenCode route success rate | 17-24% (rate limited) | 3,756 Fabric attempt receipts |
| Claude cooldown scope | whole adapter, although accounts differ | `provenant routes` |

## Decisions

User decisions, 2026-10-21:

| ID | Decision |
|---|---|
| D1 | Autonomy is a per-request stance, not a mode. The chair infers `pair`, `outcome` or `drain` from how the user phrases each request, switches freely, and records the current stance in its state file only as a hint that survives compaction. Merging stays agent-owned in every stance. |
| D2 | The implementing tier follows task size and triviality, not a fixed default. Juniors write when that suits the work. No bake-off now; lanes record tier, size and outcome so thresholds tune from ordinary work. |
| D3 | Routing is enforced by injection plus a soft gate, with role-typed agents as the easy path (see R1). |
| D4 | Large work decomposes down contract-driven trees (principal to seniors to juniors, review on the way up), with flexibility preserved: decomposition can adapt mid-run. |
| D5 | Claude nodes are hybrid: native subagents when the user wants to watch or the node is short and interactive; headless through Fabric when the node is long or waiting. |
| D6 | Chair account switching is fully automatic. |
| D7 | Telemetry is local-first: record everything now, optimise later. A hosted live-trace layer for Sealle is a later option. |
| D8 | Jev (TypeSafe AI) is not adopted now; revisit as a shadow classifier once telemetry yields labelled routing decisions. |

Principal-settled from evidence (the user may overrule any): keep
`autoCompactWindow` at 250k; no blanket one-hour subagent cache; agy gets no
web-research work; stale and failing routes are demoted or retired.

## Requirements

### R1 Routing policy, injection and soft gate

1. One machine-readable policy owns the size and triviality table and the
   role-to-tier map. `provenant routes` remains the live roster. Skills point
   at both and stop restating pools or model names.
2. Default table (thresholds are initial values that telemetry tunes):

   | Work | Writes |
   |---|---|
   | Bounded, objective gate available, about 3 files or fewer, no new interface, routine or substantial-low risk | junior |
   | Multi-file, ambiguous, new interface, or needs judgement | senior |
   | Cross-cutting, crucial or terminal tier, UI direction or taste | principal |
   | Scouting, mining, lens review, mechanical edits, test scaffolds | junior |
   | Decisions, risky review, synthesis | principal |

   A failed gate escalates one tier. Delegating down is right only when the
   brief is much smaller than the work and carries an executable contract;
   otherwise the tier writes it.
3. A SessionStart hook (startup, resume, clear, compact) injects the live
   roster and the table in no more than about 500 tokens.
4. A PreToolUse hook on the Agent tool and on `fabric_dispatch` bounces once,
   with the reason, a spawn that omits a model, puts a principal model on a
   non-principal role, or routes a Claude model through Fabric outside R2's
   headless node rule. Re-issuing with a one-line `principal: <why>` (or
   equivalent) passes and is logged. The gate never blocks twice.
5. Role-typed agents ship for the common roles (`scout`, `implementer`,
   `reviewer`, `lander`, `lead`) with model, effort and cache TTL in
   frontmatter. Leads and landers default to a one-hour cache.

Acceptance: over two weeks of ordinary Sealle work, Opus share of implementer
subagent tokens falls below 15%; user routing reminders fall by at least 75%
against baseline; no spawn without a model.

### R2 Contract-driven plan graphs in Fabric

1. Fabric executes a plan graph: nodes with `id`, role or tier, route or model,
   brief, parent outputs, a gate command, and `on_fail` (`repair`,
   `escalate`, `stop`). The runner, not a model, waits on children.
2. A node may be an LLM lead whose output is a sub-graph, so adaptive
   decomposition is a node type rather than a separate mechanism.
3. The chair (Claude or Codex) can add, cancel or re-route nodes while a graph
   runs.
4. Review nodes above a set of children check integration against their own
   contract, after the children's objective gates pass.
5. Depth is at most three tiers below the chair.
6. Claude nodes: headless nodes run through the Fabric Claude adapter on a
   ledger-chosen account. Their session id appears in `provenant lanes`, so
   the user can follow one live (`tail -f` on its transcript) or open it with
   `claude --resume <id>`. Native nodes are marked chair-executed and the
   chair spawns them.
7. Workflow scripts remain a Claude-native convenience; cross-family legs no
   longer need a Claude wrapper agent.

Acceptance: a three-tier graph with mixed providers runs end to end with no
idle LLM coordinator; every node writes a receipt; a mid-run edit takes
effect; a failing leaf escalates one tier.

### R3 Quota ledger and accounts

1. A ledger keyed by provider, account and host records each window's used
   share, reset time and source: Claude statusline `rate_limits`, Codex
   `account/rateLimits/read`, OpenCode Go estimated from receipts at listed
   cached and uncached prices, agy observed from limit errors.
2. Cooldowns apply per account, not per adapter.
3. Each Claude account has its own `CLAUDE_CONFIG_DIR`; skills, settings,
   hooks and agents are shared from one source; `projects/` is shared so
   sessions resume across accounts.
4. Placement spends the account whose weekly reset comes sooner first and
   keeps the later one in reserve; five-hour windows break ties.
5. At a threshold (initially 92% of either window), the chair checkpoints and a
   wrapper relaunches it on the other account with `claude --resume`, then
   tells the user in one line. Fully automatic per D6.
6. Accounts are added or retired by configuration only. The current Pro
   account is one ledger row.

Acceptance: with one account exhausted, new lanes and the chair continue on
another without user action; `provenant routes` shows availability per
account; user `/usage` checks fall by at least 75% against baseline.

### R4 Local telemetry ledger

1. `provenant telemetry` incrementally ingests, per host, Claude transcripts
   (deduplicated by `message.id`), Codex rollouts and state, the OpenCode
   database, Fabric receipts and the quota ledger into one local database,
   keyed by session, lane, route, account, host and repository revision.
2. `provenant stats` reports at least: cost per accepted task, cold-cache
   rewrites, tier and role mix, first-pass gate rate, escalations, user
   interventions, and quota burn per account.
3. Each Claude node records five-hour usage before and after, so headless and
   native quota cost can be compared on the user's own accounts.
4. No data leaves the user's hosts.

Acceptance: the baseline table above can be regenerated by one command, and a
known run's figures reconcile with its receipt.

### R5 Cache and context hygiene

1. No subagent or lane blocks in a foreground wait longer than about four
   minutes; long waits belong to the runner, the chair, or a background task.
2. Bash output over a bound goes to a file and returns a pointer; polling
   prints one line.
3. Startup: HARNESS.md loads on trigger, not by import; duplicate global
   instruction injection is removed; long skill descriptions are trimmed; the
   hard-coded model catalogue in orchestrate references is replaced by a
   pointer to `provenant routes`.
4. The PreCompact hook never blocks idle compaction.

Acceptance: cold rewrites fall below 15% of cache writes; subagent first-call
context p50 falls; startup context in Provenant and Sealle sessions falls by
at least 5k tokens.

### R6 Stance and continuity

1. The chair's state file carries the current stance (D1), the live lanes and
   the direction, and the post-compaction hook re-injects them, so the user
   never has to type `/compact continue <process>`.
2. In `pair` the chair asks at direction forks; in `outcome` and `drain` it
   records assumptions and continues, stopping only at one-way doors, budget
   breaches and failed gates; `drain` posts a digest at intervals.

Acceptance: manual re-primes after compaction fall to near zero; autonomy
nudges fall by at least 50% against baseline.

### R7 Knowledge and catalogue hygiene

1. Sealle preferences held in private memory move into the Sealle repository's
   docs and skills; promoted memory files are deleted, per HARNESS.md.
2. Conflicting memory rules (Astra use, Gemini Pro role, lane runtime) are
   resolved once with the user and recorded where they belong.
3. Route pools drop retired routes and demote routes whose recent success rate
   is poor; health comes from receipts.

## Delivery order

1. R5 and R7 (cheap, immediate savings, no new mechanism).
2. R1 (largest cost and attention lever).
3. R4 then R3 (the ledger depends on telemetry sources).
4. R6.
5. R2 (largest build; uses R1's policy and R3's placement).

## Parked

- Jev shadow evaluation (D8).
- Hosted observability for Sealle (D7).
- Autonomous merging of retrospect-proposed harness changes; until decided,
  those changes are PRs for user review.
- A junior-versus-senior implementer bake-off (D2).

## Non-goals

- Disguising traffic or rotating accounts to evade provider controls.
- Replacing native subagents for interactive work.
- Sending Sealle content to a third-party classifier or trace service.
