---
name: code-review
description: "Use for independent read-only review of PRs, diffs, commits and refactors against the spec, through review-council or provenant review. Not for root cause (diagnose), fixes (implement) or rendered UI (ui-ux-design)."
---

# Code review

With little or no human diff review, a clean verdict from a fresh reviewer plus
CI on the exact head is the check on the code. So the reviewer must be
independent and must prove its coverage, and "clean" must mean something.

Conventions and the review ladder come from the repo's own `AGENTS.md`,
`CONTRIBUTING` and runbooks. Look there first.

## How it runs

```sh
provenant run --preset review-council --prompt-file BRIEF.md \
  --set 'parameters.subject.commit="SHA"' --set 'parameters.base="origin/main"' \
  --set 'parameters.spec="#N"' --set 'parameters.risk_tags=["security"]' --explain
```

Lenses are `correctness-spec` (always), `design-fit-reuse`, `simplicity`,
`conventions-readability`, `tests`, then `security-privacy`,
`concurrency-data-errors` and `adversarial` by risk tag. The routing review
shape (`quick`, `standard`, `deep`, chosen from `risk_tier`) decides who
reviews. A verifier tries to falsify each finding and a senior synthesis gives
each a disposition. For a quick fan-out without a graph:
`provenant review TARGET --brief-file B --shape quick|standard|deep`.
`deliver` already runs this council on the newest head.

## Independence

- The reviewer is read-only and reads the pinned commit with `git show` and
  `git diff`, never the working tree, and uses no Git write verbs.
- The brief carries the subject, the base and the spec. Never send the
  author's hypothesis, expected verdict or scratchpad.
- Use a distinct model family from the author when one is available; the
  HARNESS.md review ladder requires it for `substantial` and higher work. When
  none is available, require an independent same-family reviewer through a
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

Start with a line `Verdict: approve`, `request_changes` or `inconclusive`;
graph nodes end with `VERDICT: PASS` or `VERDICT: FAIL`. The author marks each
finding accepted, rejected (with evidence) or deferred. One repair follows and
re-review covers only that repair. `clean` is a valid verdict.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, migrations), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that acceptance
criteria map to evidence and that no test was loosened, skipped or deleted.
Inspection questions are in [review-lenses.md](references/review-lenses.md).
Reviewer-facing context is [reviewer-context.md](references/reviewer-context.md);
the lens prompts under `references/lenses/` load into the presets by path.

## Boundaries

`orchestrate` owns topology and scheduling; `diagnose` owns root cause;
`implement` owns repairs. For rendered-UI changes also inspect light and dark
captures of changed screens (`ui-ux-design`).
