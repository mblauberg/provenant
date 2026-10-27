# Tiered council

When several model families are available, work as a hierarchy, like an
engineering team. Juniors from many families do the laid-out work and the
fine-grained passes. Seniors judge that work, make the medium decisions and
delegate to juniors. Principals make the critical calls. `junior`, `senior` and
`principal` are ordered route pools, so one task names one pool. The `writing`
and `design` pools are craft choices, not extra risk tiers. `provenant route
list` shows members in preference order.

| Tier | Route | Job |
|---|---|---|
| Junior | `junior` | laid-out work and lens passes that need little judgement; overlap freely |
| Senior | `senior` | judge the work and the junior claims; medium decisions; delegate to juniors |
| Principal | `principal` | critical decisions and high-level judgement |

Rust routing resolves a pool in order and does not rotate members. A repeated
`route: "junior"` can select the same model each time. To seat distinct
families, inspect the pool and submit a separate task for each exact model.
Claude chairs use native Claude subagents. Claude models stay in the pools for
non-Claude chairs, which can dispatch them through Fabric.

| Craft pool | Route | Job |
|---|---|---|
| Writing | `writing` | writing and accuracy work |
| Design | `design` | visual and design judgement |

## Depth follows risk

Use the `HARNESS.md` risk tier as the mode, not a separate switch:

- `routine`: no hierarchy; one lane or the chair alone.
- `substantial`: juniors and seniors.
- `crucial` and `terminal`: all three tiers, ending in principal consensus and
  one synthesis.

## Bottom-up: review, research, visual review

1. **Split.** Cut the subject into parts small enough for one junior each:
   files, screens, claims or sources.
2. **Juniors.** Submit one task per selected junior model for each part. A batch
   may carry several tasks, but the chair chooses distinct exact models when
   cross-family cover is needed; the router does not fill or diversify a
   council. Juniors return claims with evidence (path:line, screenshot, source),
   not verdicts.
   Review briefs follow the [review outcome](../../code-review/references/finding-contract.md#review-outcome) rule.
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

- Free OpenCode models sit last in the pools and require explicit free opt-in.
  They do not rotate automatically. If a typed failure permits rerouting, the
  chair chooses and records the next exact model.
- The Rust Fabric task schema has no `confidential` field. For a CLI
  eligibility check, run `provenant route resolve --route NAME --confidential`;
  the resolver excludes models without `trains_on_prompts = false`. Inspect
  each model's current catalogue row before selecting a confidential route.
- A council of N is N explicit model tasks. Verify each task's resolved route;
  do not count repeated selections as distinct family coverage.
- Run the full hierarchy per wave (an integrated branch, a combined capture
  set), not on every small fix. A nit-only follow-up gets the chair's own read.
