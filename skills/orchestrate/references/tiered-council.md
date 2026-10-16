# Tiered council

When several model families are available, work as a hierarchy. Many cheap
lenses from different families cover the fine grain. Mid-tier judges weigh the
work together with the lens claims. The top tier decides. Each tier is a
route pool, so a dispatch names its tier directly:

| Tier | Pool | Members (seed) | Job |
|---|---|---|---|
| 1, lenses | `bulk` | DeepSeek and GLM free, Haiku, Luna, Gemini Flash (low/medium), rotating free models at `sparing` | laid-out work needing little judgement; overlap freely |
| 2, judges | `workhorse` | Sonnet, Sol, Gemini Flash (high), agy Opus 4.6 at `sparing` | judge the work and the tier-1 claims; medium decisions; delegate to tier 1 |
| 3, deciders | `flagship` | Opus, Astra, Argon at `sparing` | critical decisions, high-level judgement, consensus |

`strong` and `review` map to `workhorse`; `scout`, `mechanical` and `lens` map to
`bulk`. A Claude chair runs its own Claude members as native subagents: a pool
hands them back with a spawn line ([routing-and-tiers.md](routing-and-tiers.md)).

## Depth follows risk

Use the `HARNESS.md` risk tier as the mode, not a separate switch:

- `routine`: no hierarchy; one lane or the chair alone.
- `substantial`: tiers 1 and 2.
- `crucial` and `terminal`: all three tiers, ending in consensus and one
  synthesis.

## Bottom-up: review, research, visual review

1. **Split.** Cut the subject into parts small enough for one lens each: files,
   screens, claims or sources.
2. **Tier 1.** Give each part its own council, for example `route: "bulk",
   council: 3`. Councils seat new families first, so each part gets cross-family
   cover for blind spots. Lenses return claims with evidence (path:line,
   screenshot, source), not verdicts.
3. **Tier 2.** Each judge works separately over a wider scope (several parts or
   the whole) and receives the subject plus the tier-1 claims for that scope.
   It confirms, rejects or merges each claim and adds its own view.
4. **Tier 3.** Each decider reads everything, including every tier-2 review,
   and the deciders work towards consensus. Disagreements are settled on
   evidence, never by vote.
5. **Synthesis.** One fresh top-tier agent writes the outcome with the reasons
   ("the why"), so implementers know what to do and why.

Pass structured handoffs between tiers ([layering-and-context.md](layering-and-context.md)):
each claim keeps its id, evidence and origin lens, so a higher tier can cite and
overrule it.

## Top-down: implementation

1. **Tier 3** scopes the change and decides the approach and slice boundaries.
2. **Tier 2** turns each slice into a precise brief and owns its medium
   decisions.
3. **Tier 1** implements the laid-out slices in partitioned worktrees.
4. Checks flow back up: tier 2 reviews its slices, and tier 3 accepts the whole.

## Practical limits

- Free tiers rotate and rate-limit. Spread tier-1 lenses across adapters rather
  than stacking one free model, and treat `rate_limited` as a re-route signal.
- Keep private or client data off prompt-training models: pass
  `confidential: true` and the pools skip them.
- A council of N reuses members once the pool's available models run out, with
  a warning; widen the pool or accept the repeats.
- Run the full hierarchy per wave (an integrated branch, a combined capture
  set), not on every small fix. A nit-only follow-up gets the chair's own read.
