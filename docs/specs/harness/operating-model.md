# Spec: harness operating model for routing, cache, quota and telemetry

Status: **accepted** by the user on 2026-10-21 (r2, after a
`codex/gpt-6.1-sol@high` review); r3 folds in the user's review answers. GitHub owns delivery state; this file owns
the durable requirements. It grants no authority beyond what the user approves
by accepting it, and it does not relax any gate in `HARNESS.md`.

## Outcome

The user scopes outcomes and steers; the harness does the rest without being
reminded. Concretely:

- the chair picks shape, tier, model and effort from policy it is given, not
  from memory it has to be prompted to recall;
- work spreads across every provider and account the user holds, paced to
  each pool's reset cycle, on both hosts;
- tokens go to useful work rather than cold-cache rewrites, idle coordinators
  and oversized tool output;
- external lanes get the tools they need inside an enforced box, instead of
  prompt-level restrictions that cripple them;
- every run leaves joined, deduplicated telemetry, so later retrospection
  works from data rather than transcripts.

## Evidence baseline (60 days to 2026-10-21)

The study artefacts are local and listed in the R4 source manifest once it
exists. Until then these figures are indicative; R4 regenerates them
reproducibly and acceptance is measured against the regenerated values, as
rates rather than raw counts.

| Signal | Baseline | Source |
|---|---|---|
| Subagent share of Claude tokens | 77% | Claude transcripts, deduped by `message.id` |
| Sealle Opus subagent tokens | 25.9B; 89% from an explicit `model: "opus"` on the spawn | spawn metadata |
| Opus subagent tokens by role | implementer 44%, coordinator 22%, reviewer 13%, researcher 10%, lander 4% | role classifier, 150-file hand check |
| Cold cache rewrites (gap over 5 min) | 42% of Sealle cache writes, about 710M tokens | gap audit |
| Gap causes | foreground Bash wait or poll 43%, other long Bash 41%, end of turn 15% | gap audit |
| Compaction cost | 1,918 compactions, about 58M tokens | compact boundaries |
| Bash share of tool-result characters | 85% | Claude transcripts |
| Subagent first-call context | p50 36k (main 11k) | Claude transcripts |
| User routing reminders | about 13% of 3,518 user messages | user-message classifier |
| User autonomy nudges | about 17% of user messages | user-message classifier |
| Manual `/compact continue` re-primes | 349, against 662 automatic compactions | user messages |
| User `/usage` checks | 146 | user messages |
| Free OpenCode attempts ending `rate_limited` | most non-ok outcomes on DeepSeek and GLM free | 3,756 Fabric attempt receipts |
| Cooldown scope | adapter and model; `account` always null (`fabric_records.py:404-429`) | code |

## Decisions

User decisions, 2026-10-21:

| ID | Decision |
|---|---|
| D1 | Autonomy is a per-request stance (`pair`, `outcome`, `drain`) the chair infers from phrasing and records only as a hint. Stance changes how often the chair checks in, never which gates apply. |
| D2 | The implementing tier follows task size and triviality. Juniors write when that suits the work. No bake-off now; lanes record tier, size and outcome so thresholds tune from ordinary work. |
| D3 | Routing is enforced by injection plus a soft gate, with role-typed agents as the easy path. |
| D4 | Large work decomposes down contract-driven trees with review on the way up, and decomposition can adapt mid-run. |
| D5 | Claude nodes are hybrid: native when the user wants to watch or the node is short; headless through Fabric when it is long or waiting. |
| D6 | Account switching is automatic through one provider-neutral account pattern, enabled for every provider the user holds several accounts with (R3.7). |
| D7 | Telemetry is local-first: record everything now, optimise later. |
| D8 | Jev (TypeSafe AI) is not adopted now; revisit as a shadow classifier once telemetry yields labelled routing decisions. |
| D9 | Monthly-reset pools are paced, not retired. OpenCode free models go first and Go paid is conserved. Cursor is depleted until its monthly reset (2026-10-21); its seats go to Sonnet or Sol meanwhile, and Grok is used sparingly afterwards. |
| D10 | DeepSeek V4.1 Flash sits between junior and senior and is the default first writer for bounded work on its free tier. On a free rate limit, work moves to other free non-training models, then to subscription juniors; Go paid comes last and is paced, so free rate limits do not drain it. |
| D11 | Confidentiality keys on whether a model trains on prompts, not on price. DeepSeek V4.1 Flash and GLM-5.3 Flash free on OpenCode do not train and are eligible for confidential work. |
| D12 | Astra is a blind-spot challenger on crucial and terminal calls only: about five times Sol's cost for a marginal gain, but good at finding what others miss. |
| D13 | Harness changes proposed by retrospect are merged by agents when routine, because the user scoped them first; the rest receive tier-required agent review. Explicit user acceptance applies only to `terminal` work or approved scope; one-way doors and external actions remain user gates. |
| D14 | Research is under-used. Research lanes get network access and the chair dispatches them proactively whenever a decision rests on external facts. |
| D15 | R9 rolls out without a separate probe phase; the box's own deny tests ship in the same change. |

Principal-settled from evidence (the user may overrule any): keep
`autoCompactWindow` at 250k; no blanket one-hour subagent cache; agy gets no
web research until R9 gives it a working network path.

## Requirements

### R1 Routing policy, injection and soft gate

1. One machine-readable policy file owns the size table, the role-to-tier map
   and per-pool pacing (R3). `provenant routes` stays the live roster. Skills
   point at both and stop restating pools or model names.
2. Default table (initial thresholds; R4 data tunes them):

   | Work | First writer | Escalates to |
   |---|---|---|
   | Bounded, objective gate, about 3 files or fewer, no new interface, routine risk | DeepSeek V4.1 Flash free; on rate limit another free non-training model, then Luna, Haiku or Gemini Flash; Go paid last | senior |
   | Multi-file, ambiguous, new interface, or judgement | senior | principal |
   | Cross-cutting, crucial or terminal risk, UI direction or taste | principal | user |
   | Scouting, mining, lens review, mechanical edits, test scaffolds | junior | senior |
   | Research on external facts (docs, papers, prices, prior art) | several juniors with network, fanned out | senior synthesis |
   | Decisions, risky review, synthesis | principal | user |

   A failed gate escalates one tier after at most one repair attempt at the
   current tier. Delegating down is right only when the brief is much smaller
   than the work and carries an executable contract (failing test, types or
   check command).
3. Each primary harness injects the live roster and table, in at most about
   500 tokens, at start and after compaction: Claude through a SessionStart
   hook (startup, resume, clear, compact); Codex through its equivalent hook
   where one exists, otherwise through the chair-state checkpoint that
   `skills/session/references/chair-state.md` already defines for hookless
   hosts.
4. A soft gate checks spawns before execution: the Claude PreToolUse hook on
   the Agent tool and `fabric_submit`, and the Fabric dispatch path for
   every chair. It bounces once, with the reason, a spawn that names neither
   a tier nor a model, puts a principal model on a non-principal role, or
   breaks pacing. A reissue carrying `principal: <why>` passes and is logged.
   Tier selectors are valid input; the resolved model is always recorded in
   the receipt. The override waives routing preference only, never
   authority, confinement, confidentiality or assurance gates.
5. Role-typed agents ship for `scout`, `implementer`, `reviewer`, `lander`
   and `lead`, with model, effort, cache TTL and a tool allowlist in
   frontmatter. Leads and landers default to a one-hour cache.
6. Confidential routing checks each model's training flag in the catalogue,
   citing the provider's statement. Free and non-training models (D11) are
   eligible. Sealle's tracked controls (`opencode.json`, fabric policy,
   `pnpm check:confidential-content`) change through a Sealle PR to match.

Acceptance (two consecutive weeks of ordinary Sealle work, against the R4
baseline): Opus share of implementer subagent tokens below 15%; routing
reminders below 3% of user messages; every receipt records a resolved model.

### R2 Contract-driven plan graphs

This requirement amends the non-goal in
[provider-execution-core.md](provider-execution-core.md) that excludes a
scheduler. The new owner is a **graph runner** in the Fabric runtime. It
schedules only; provider mechanics stay with the existing adapters, and the
lifecycle chain in `HARNESS.md` is unchanged. The existing `depends_on`
field is relationship metadata (`store.ts:872-889` claims without checking
it); the runner adds enforcement.

1. A graph is a versioned document of nodes. Each node has `id`, role or
   tier, route or model, execution mode, writer worktree (if writing),
   inherited authority (narrowing only), brief, parent outputs, a gate
   command, and `on_fail` (`repair`, `escalate`, `stop`). The runner waits on
   children; no model sits idle waiting.
2. A lead node may return a sub-graph, so adaptive decomposition is a node
   type. Depth is at most three tiers below the chair.
3. The chair (Claude or Codex) edits a running graph by publishing a new
   revision. Cancelling or rerouting a running node completes only once its
   process has exited (per `worker-liveness.md`); a replacement starts only
   after that. Results carry their graph revision and node attempt, and the
   runner rejects stale ones.
4. One writer per worktree holds throughout. Review nodes above a set of
   children check integration against their own contract after the
   children's gates pass.
5. Repair is bounded at one attempt per tier. A failure at the principal
   tier stops the branch and reports to the chair.
6. Headless Claude nodes need an explicit `headless` execution selector.
   The `native_seat_model` refusal (`execution-input.ts:340-345`) gains a
   narrow exception for that selector only; interactive Claude dispatch
   through Fabric stays refused. Tests cover the refusal, an eligible
   headless dispatch and a mixed batch. A headless node's session id appears
   in `provenant lanes`, so the user can follow it live or open it with
   `claude --resume <id>`.
7. Lanes are registered under the dispatch `cwd`'s project, not the chair's
   start directory, so `provenant lanes --wait` from a repository sees lanes
   dispatched into it. (Observed twice on 2026-10-21: a chair started in `~`
   filed worktree lanes under `~`, and `lanes --wait` reported none.)

8. **Templates with overrides.** Graphs are built from named templates, so
   the common case is one line and anything is customisable:

   | Template | Shape | Default from |
   |---|---|---|
   | `single` | one writer, gates, review | routine work |
   | `fanout` | N parallel juniors, one senior merge | wide mechanical or research work |
   | `council` | N lenses across families, principal synthesis | decisions, risky review |
   | `tree` | lead emits sub-graphs from any template | large or unclear work |
   | `pipeline` | each item flows through stages without waiting for the batch | per-item work at scale |

   The size class and risk tier pick the template and its defaults (width,
   tiers, routes, gates, review lenses, `on_fail`). Any field can be
   overridden per graph or per node, templates nest, and a graph can be
   saved as a named template. Width scales to the ledger's live capacity
   per provider and account, within a per-graph cap.
9. **Review and gate ordering.** Gates split into fast checks (lint, types,
   targeted tests), which block before review, and slow checks (full suites,
   CI), which run in parallel with review. Findings from both feed one
   repair round, and re-review covers only the repair delta. Every gate still passes before merge, as
   `HARNESS.md` allows for owner-set test cadence. A node whose
   change affects UI also produces screenshots (Playwright captures at set
   viewports) and attaches them to its review brief; reviewers with vision
   get them.
10. **Context budget per lane.** Lanes are sized by context, not minutes: one
    logical unit, one commit. A lane checkpoints its progress to a notes file
    as it goes; when it nears its context budget it ends, and the runner
    restarts it fresh from the brief, contract and notes, so it never works
    from a rotted context.

Acceptance: a three-tier mixed-provider graph runs end to end with no idle
model coordinator; a template graph runs from a one-line request and from a
node-level override; every node writes a receipt; a mid-run revision takes
effect with no stale result accepted; a failing leaf repairs once then
escalates; a cancelled writer exits before its replacement starts.

### R3 Quota ledger, pacing and accounts

1. A ledger keyed by provider and account records each window's used share,
   reset time, source and observation time: Claude statusline `rate_limits`;
   Codex `account/rateLimits/read`; OpenCode Go estimated from receipts at
   listed cached and uncached prices against its five-hour, weekly and
   monthly dollar limits; Cursor and agy observed from limit errors. Host is
   recorded as provenance; stale observations (older than one window) do not
   drive placement.
2. Cooldowns key on provider, account and model. Only a usage-limit outcome
   (`usage_limited`) cools an account. A provider-wide throttle, an
   authentication failure or a policy refusal does not trigger an account
   switch.
3. Pacing per pool:
   - free OpenCode models first for eligible work; `rate_limited` is a short
     per-model cooldown and a retry, not a failure, and does not lower the
     route's health;
   - OpenCode Go paid only after the free model fails, paced so the monthly
     limit is not spent faster than its remaining days allow;
   - Cursor Grok at most one seat per council, paced to its monthly reset;
   - between Claude accounts, spend the one whose weekly reset comes sooner
     first, with five-hour windows breaking ties.
4. Each Claude account has its own `CLAUDE_CONFIG_DIR` with subscription
   login. `ANTHROPIC_API_KEY` is unset for these launches, because the adapter
   otherwise prefers it (`adapters/claude.py:15-24`). Skills, settings, hooks
   and agents are shared from one source. The shared `settings.json` link means
   settings changed in one account apply to all linked accounts, and if Claude
   replaces the link with a regular file the accounts stop sharing; rerunning the
   installer does not overwrite it.
5. Cross-account resume needs proof before any store migration: a
   demonstration that a session started under one config directory resumes
   under the other, with a backup and rollback of both `projects/` stores.
   If it cannot be shown, the switch starts a fresh session from the chair
   checkpoint instead.
6. Chair switching at a threshold (initially 92% of either window): the chair
   checkpoints, takes a single-writer lock on its session, exits, and a
   wrapper relaunches it on the other account, then reports in one line.
   The old process must have exited before the new one starts.
7. **Account pattern and terms boundary.** Each provider adapter declares
   how an account is isolated (Claude `CLAUDE_CONFIG_DIR`, Codex
   `CODEX_HOME`, and the verified equivalent for agy and OpenCode), where its
   quota is read, and how a lane or chair moves to another account. Switching
   is enabled per provider by configuration; the user enabled it for all
   providers on 2026-10-21. Every switch stays inside these rules:
   - each account is signed in by the user through the provider's own flow;
     credentials are never copied between identity directories or handed to
     another tool;
   - a switch moves work to an account with its own headroom; no account is
     pushed past its limit and no limit signal is suppressed;
   - accounts are the user's own and used only by the user's agents.

   OpenCode's terms name "multiple accounts to circumvent usage limits" as
   grounds for termination; the user accepted that risk on 2026-10-21, and
   the adapter documents the clause beside its flag.
8. Accounts are added or retired by configuration only.

Acceptance: `provenant routes` shows availability per account; with one
Claude account exhausted, new lanes place on another eligible account
without user action; `/usage` checks fall below one per active day.

### R4 Local telemetry ledger

1. `provenant telemetry` incrementally ingests, per host, Claude transcripts
   (deduplicated by `message.id`), Codex rollouts and state, the OpenCode
   database, Fabric receipts and the R3 ledger into one local database, keyed
   by session, lane, graph node, route, account, host and repository
   revision.
2. A versioned source manifest names every input, its parser version and the
   classifier versions used for user-message categories, so any figure can be
   regenerated.
3. Cost is allocated per receipt from listed prices (cached and uncached
   separately). An accepted task is one whose project landing gate passed, or whose
   objective checks and tier-required review met the approved criteria.
   Explicit user acceptance is required only for `terminal` work or when
   the approved scope asks for it; Fabric `ok` alone is an execution outcome, not
   acceptance.
4. `provenant stats` reports at least: cost per accepted task, cold-cache
   rewrites, tier and role mix, first-pass gate rate, escalations, user
   interventions per hundred messages, cache-read share per provider, and
   quota burn per account.
   Owner requirement, 2026-10-21: the ledger supports evaluating per-model
   effectiveness (gate pass, acceptance and cost per accepted task), graph
   efficiency once graphs exist, routing quality (chosen route versus
   outcome, fallbacks and escalations), and provider pacing. Pacing records
   observed usage per provider, account and window: for example, Cursor usage
   against its monthly allowance, and OpenCode free-model request rate
   against rate-limit incidence so bursts can be smoothed. These measures
   support continuous improvement of routing and spend.
5. Quota deltas around a Claude node are account-level aggregates and are
   labelled as such. Native versus headless quota comparisons use a
   controlled run with one node per account at a time.
6. No data leaves the user's hosts.

Acceptance: one command regenerates the baseline table within 2% of each
figure; a known run's figures reconcile with its receipts.

### R5 Cache and context hygiene

1. No subagent or lane blocks in a foreground wait longer than about four
   minutes; long waits belong to the runner, the chair, or a background task.
2. Bash output over a bound goes to a file and returns a pointer; polling
   prints one line.
3. Briefs are cache-shaped for every provider: a stable shared preamble and
   context pack first, task-specific content last, and follow-ups resume the
   lane's session rather than starting fresh. Sibling graph nodes share one
   context pack. This matters most where cached reads are far cheaper than
   input: DeepSeek V4.1 Flash and MiMo on OpenCode Go (about 50 times), GLM
   (about 5 times), Claude Opus (about 20 times), Codex (automatic prefix caching).
4. Startup (variable part about 12k tokens today, on a 52k first call):
   - HARNESS.md loads on trigger, not by `@` import (about 2k);
   - the global instructions are injected once, not through two symlinks
     (about 0.5k);
   - rarely used skills are listed by name only and unused ones are
     disabled; long descriptions are cut to about 200 characters (the skill
     listing is the largest part, about 6.5k);
   - MCP servers are scoped to the projects that use them (AWS tools only
     where AWS work happens);
   - repeated policy in `AGENTS.md` files moves to docs behind a pointer;
   - the hard-coded model catalogue in
     `orchestrate/references/routing-and-tiers.md` becomes a pointer to
     `provenant routes`;
   - role agents (R1.5) carry tool allowlists, so subagents load fewer tool
     schemas than a general-purpose agent.
5. The PreCompact hook never blocks idle compaction.

These ship immediately. Acceptance is measured once R4 exists: cold
rewrites below 15% of cache writes; subagent first-call context p50 down;
variable startup context in Provenant and Sealle sessions below 6k tokens;
role-agent first-call context below the general-purpose p50.

### R6 Stance and continuity

1. The chair's state file carries the current stance (D1), live lanes and
   direction. Post-compaction injection (per R1.3's per-harness mechanism)
   restores them, so the user does not retype `/compact continue <process>`.
2. In `pair` the chair asks at direction forks. In `outcome` and `drain` it
   records assumptions and continues between gates, and `drain` posts a
   digest at intervals. In every stance the constitutional user gates hold:
   specs and one-way doors, risk-tier downgrades, unresolved acceptance
   criteria, production promotion, destructive or
   irreversible actions and external communications. Final acceptance is met
   by objective checks and tier-required review; explicit user acceptance is
   required only for `terminal` work or when the approved scope asks for it.
   The chair reports the outcome and the user may reject it. A project may
   name landing as acceptance (Sealle: landing is acceptance for everything).
   Merging follows each
   repository's own authority and workflow (Sealle: the lander through
   `tools/process/land.mjs`).

Acceptance: manual re-primes below 5% of compactions; autonomy nudges below
8% of user messages.

### R7 Knowledge and catalogue hygiene

1. Sealle preferences held in private memory move into the Sealle
   repository's docs and skills; promoted memory files are deleted, per
   HARNESS.md.
2. Conflicting memory rules (Astra use, Gemini Pro role, lane runtime) are
   resolved once with the user and recorded where they belong.
3. Route health comes from receipts and separates `rate_limited` and
   `usage_limited` from real failures. Routes are demoted for real failures
   only; pacing (R3.3) handles limits. Stale model ids are removed from
   catalogues and memories.

### R8 Skill changes

| Skill or file | Change |
|---|---|
| `orchestrate` | Graph templates and overrides (R2.8); context budgets and lane notes (R2.10); research fan-out with network (D14). Replace the model catalogue with pointers to `provenant routes` and the policy file (R1.1); add the size table, contract handoffs and escalation (R1.2); cache-shaped briefs and session reuse (R5.3); no foreground waits over four minutes (R5.1); graph runner usage (R2); shrink per-provider prompt boilerplate once R9 boxes exist. |
| `session` | Stance in chair state; post-compaction re-injection; PreCompact never blocks (R5.5, R6). |
| `implement` | Writer tier by size; one repair, then escalate; contracts on every downward handoff; fast gates before review, slow gates in parallel with it; screenshots for UI changes (R2.9). |
| `scope` | Emit a size class alongside the risk tier, so routing has an input. |
| Role agents | New `scout`, `implementer`, `reviewer`, `lander`, `lead` definitions (R1.5); Sealle's existing `sealle-lander` becomes the template. |
| Sealle `sealle-coordinate` | Absorbs promoted routing, landing and roster memories (R7.1). |
| Vendor skills | Not edited; listed by name only or disabled when unused. |
| Sealle confidentiality controls | Sealle PR marks the non-training free models eligible (D11). |

### R9 Contained freedom for external lanes

External CLIs are currently restricted by prompt text and per-CLI flags,
which both cripple them and fail open. Read-only agy runs with `--sandbox`,
so it has no usable shell, git or network, returns thin answers and has
fabricated citations; OpenCode needs a multi-line "stay inside this
checkout" brief and a `cwd` outside the repository, and a denied edit still
wrote a file. Writer agy lanes already run with
`--dangerously-skip-permissions` (`adapters/agy.py:22-23`).

1. Every external lane, read-only or writer, runs inside an enforced box: a
   disposable linked worktree or scratch copy as its only writable area, and
   an OS-level policy that denies reads of credential stores (`~/.ssh`,
   `~/.aws`, `~/.kube`, `~/.secrets`, Keychain), `~/Monero` and
   `~/Archives/git-preservation`, and denies writes outside the box and the
   CLI's own state directory.
2. Inside the box the CLI runs with its permissive mode (agy
   `--dangerously-skip-permissions`, OpenCode allow-all, Cursor agent mode),
   with shell, git and test commands available. Network is on for research
   and review lanes, and on for writers that need package or documentation
   access.
3. "Read-only" for an external lane means its box is discarded and only its
   result file is collected, not that the CLI is forbidden to act.
4. What stays fixed: confidentiality routing (free and training models never
   see barred content), external actions (push to shared branches, posting,
   publishing) and secrets. These are authority, not guardrails, and the box
   does not change them.
5. No separate probe phase (D15). The change that introduces the box
   carries its own deny tests: reading a denied path and writing outside the
   box fail, and a normal task succeeds. A box that fails its tests falls
   back to today's restrictions for that adapter instead of running open.

Acceptance: agy research lanes return sourced answers that pass citation
spot checks; the per-provider confinement paragraphs leave the briefs; the
probe suite runs in CI against each adapter's argv.

## Delivery order

1. **Now, no new mechanism:** R5 (except measured acceptance), R7, R8
   catalogue pointers, R2.7's lane-project fix, R3.3 pacing rules in the
   existing route policy.
2. **Sources:** R4.1-R4.3 ingestion and manifest, plus R3.1-R3.2 ledger
   collection and per-account cooldown keys.
3. **Routing:** R1 and role agents; R4.4 reports turn on, and R5's
   acceptance is measured.
4. **Freedom:** R9 probes, then boxes per adapter.
5. **Accounts:** R3.4-R3.6, behind R3.7's per-provider flags.
6. **Continuity:** R6.
7. **Graphs:** R2, then the R4.5 controlled native-versus-headless
   comparison.

## Parked

- Jev shadow evaluation (D8).
- Hosted observability for Sealle (D7).
- A junior-versus-senior implementer bake-off (D2).

## Non-goals

- Disguising traffic, sharing or copying credentials, or switching outside
  R3.7's boundary.
- Replacing native subagents for interactive work.
- Sending Sealle content to a third-party classifier or trace service.
