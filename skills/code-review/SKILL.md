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
provenant run --use review --file BRIEF.md --explain
```

Pin the base, subject commit and spec in the brief. Use `review` by default;
a dotted flavour such as `review.security` selects a specific lens. A composite
runs `review` over a panel resolved at launch, then `review.verify` and `review.synthesis`
where needed. The planner chooses lenses and tiers from risk, changed paths
and author tier; the chair checks HARNESS floors and adds missing coverage.
Review each slice lightly and the integrated head with the full required panel.
Lens breadth comes before family spread. `deliver` includes review before landing.

Inspect the explanation, then submit without `--explain`. Collect findings with
`provenant output TARGET`; use `provenant output TARGET --trace` for lineage.
A changed subject needs review of its delta against the prior findings.

## Independence

- The reviewer is read-only and reads the pinned commit with `git show` and
  `git diff`, never the working tree, and uses no Git write verbs.
- The brief carries the subject, the base and the spec. Never send the
  author's hypothesis, expected verdict or scratchpad.
- Prefer a model family distinct from every author and repairer; the
  HARNESS.md review ladder asks for it from `substantial` upward when one is
  available. When none is available, require an independent same-family reviewer through a
  different lens, without reducing the review count, and record that the
  family separation was unavailable; do not claim it.
- A review that cannot read its rubric or required context is `incomplete`,
  never `clean`.

## Rubric

- **Blocking:** a criterion fails or is left unverified, a security or
  data-loss defect, a broken public contract, or a regression.
- **Should-fix:** a concrete cost against this repo's conventions.
- **Notes:** at most three.

Each finding names the location, the failing scenario, the mechanism and the
smallest fix. A claim that something is missing or unused cites the search that
shows it. Reject outright: new abstractions, tests for states the types make
impossible, style that lint covers, and scope growth. A structural alternative
blocks only when tied to a defect. A file over 1,000 lines is a prompt to look
for a cohesion defect, not a finding.

The template engine owns the findings and verdict contract.
The author marks each
finding accepted, rejected (with evidence) or deferred. A separate writer repairs
accepted findings; the lenses that raised them re-review the delta. Follow the
HARNESS repair and redesign loop. `clean` is a valid verdict.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, migrations), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that acceptance
criteria map to evidence and that no test was loosened, skipped or deleted.
Inspection questions are in [review-lenses.md](references/review-lenses.md).
Reviewer-facing context is [reviewer-context.md](references/reviewer-context.md);
the lens prompts under `references/lenses/` supply the template briefs.

## Boundaries

`orchestrate` owns topology and scheduling; `diagnose` owns root cause;
`implement` owns repairs. For rendered-UI changes also inspect light and dark
captures of changed screens (`ui-ux-design`).
