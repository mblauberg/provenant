# Tiered council

When several model families are available, work as a hierarchy, like an
engineering team. Juniors from many families do the laid-out work and the
fine-grained passes. Seniors judge that work, make the medium decisions and
delegate to juniors. Principals make the critical calls. Each tier is a route
pool, so a dispatch names its tier directly. The live roster and named routes
are shown by `provenant route list`.

| Tier | Route | Job |
|---|---|---|
| Junior | `junior` | laid-out work and lens passes that need little judgement; overlap freely |
| Senior | `senior` | judge the work and the junior claims; medium decisions; delegate to juniors |
| Principal | `principal` | critical decisions, high-level judgement, consensus |

The same three names work as an adapter alias (`alias: "senior"`) for that
adapter's own model of the tier. Task-class names resolve: `mechanical`, `lens`
and `research` mean `junior`; `implementation` means `senior`. The retired names
`scout`, `bulk`, `workhorse`, `strong`, `review` and `flagship` are rejected,
naming their replacement. A
Claude chair runs its own Claude members as native subagents: a pool hands them
back with a spawn line ([routing-and-tiers.md](routing-and-tiers.md)).

## Depth follows risk

Use the `HARNESS.md` risk tier as the mode, not a separate switch:

- `routine`: no hierarchy; one lane or the chair alone.
- `substantial`: juniors and seniors.
- `crucial` and `terminal`: all three tiers, ending in principal consensus and
  one synthesis.

## Bottom-up: review, research, visual review

1. **Split.** Cut the subject into parts small enough for one junior each:
   files, screens, claims or sources.
2. **Juniors.** Give each part its own council, for example `route: "junior",
   council: 3`. Councils seat new families first, so each part gets cross-family
   cover for blind spots. Juniors return claims with evidence (path:line,
   screenshot, source), not verdicts.
3. **Seniors.** Each senior works separately over a wider scope (several parts
   or the whole) and receives the subject plus the junior claims for that scope.
   It confirms, rejects or merges each claim and adds its own view.
4. **Principals.** Each principal reads everything, including every senior
   review, and the principals work towards consensus. Disagreements are settled
   on evidence, never by vote.
5. **Synthesis.** One fresh principal writes the outcome with the reasons ("the
   why"), so implementers know what to do and why.

Pass structured handoffs between tiers ([layering-and-context.md](layering-and-context.md)):
each claim keeps its id, evidence and origin, so a higher tier can cite and
overrule it.

## Top-down: implementation

1. **Principals** scope the change and decide the approach and slice boundaries.
2. **Seniors** turn each slice into a precise brief and own its medium
   decisions.
3. **Juniors** implement the laid-out slices in partitioned worktrees.
4. Checks flow back up: seniors review their slices, and a principal accepts
   the whole.

## Practical limits

- Free models rotate and rate-limit. Spread junior lenses across adapters rather
  than stacking one free model, and treat `rate_limited` as a re-route signal.
- Keep private or client data off prompt-training models: pass
  `confidential: true` and the pools skip them.
- A council of N reuses members once the pool's available models run out, with
  a warning; widen the pool or accept the repeats.
- Run the full hierarchy per wave (an integrated branch, a combined capture
  set), not on every small fix. A nit-only follow-up gets the chair's own read.
