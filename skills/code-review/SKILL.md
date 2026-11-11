---
name: code-review
description: "Use for independent read-only review of PRs, diffs, commits and refactors against the spec, through the review template. Not for root cause (diagnose), fixes (implement) or rendered UI (ui-ux-design)."
---

# Code review

Review independently at a pinned head and prove coverage alongside CI.

Conventions and the review ladder come from the repo's own `AGENTS.md`,
`CONTRIBUTING` and runbooks. Look there first.

## How it runs

```sh
provenant run --use review --file BRIEF.md --set subject='"origin/branch"' --set review_base='"origin/main"' --explain
```

Supply the subject and base on the command line (a ref is enough for the base; the host computes the merge base), and put the spec in the brief. The host fetches remote subjects and supplies pinned Git context; the read-only lane cannot fetch. Use `review` by default;
a dotted flavour such as `review.security` selects a specific lens. A composite
runs `review` over a panel resolved at launch, then `review.verify` and `review.synthesis`
where needed. The planner chooses lenses and tiers from risk, changed paths
and author tier; the chair checks the path floors and adds missing coverage.
Review each slice lightly and the integrated head with the full required panel.
Lens breadth comes before family spread. `deliver` includes review before landing.

Inspect the explanation, then submit without `--explain`. Collect findings with
`provenant output TARGET`; use `provenant output TARGET --trace` for lineage.
A changed subject needs review of its delta against the prior findings and the
invariant under repair. After a shared flaw or a third round, ask whether the
design is wrong before another repair, as `implement` describes.

## Independence

- The reviewer is read-only and reads the pinned commit with `git show` and
  `git diff`, never the working tree, and uses no Git write verbs.
- The command pins the subject and base; the brief carries the spec. Never send the
  author's hypothesis, expected verdict or scratchpad.
- Prefer a model family distinct from every author and repairer when one is
  available. When none is available, require an independent same-family reviewer through a
  different lens, without reducing the review count, and record that the
  family separation was unavailable; do not claim it.
- A review that cannot read its rubric or required context is `incomplete`,
  never `clean`.

## Rubric

[reviewer-context.md](references/reviewer-context.md) defines blocking,
should-fix and note, what a finding states and what not to report. Every lens
receives it, and a manual review applies it too. A file over 1,000 lines is a
prompt to look for a cohesion defect, not a finding.

The `review` template supplies the findings and verdict protocol. Use its
evidence to judge whether the review supports acceptance; `clean` is valid when
the rubric finds no blocking defect.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, migrations), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that acceptance
criteria map to evidence and that no test was loosened, skipped or deleted to
get a green run. Inspection questions are in
[review-lenses.md](references/review-lenses.md); the lens prompts under
`references/lenses/` supply the template briefs.

## Boundaries

`orchestrate` owns topology and scheduling; `diagnose` owns root cause;
`implement` owns repairs and the chair's review loop. For rendered-UI changes also inspect light and dark
captures of changed screens (`ui-ux-design`).
