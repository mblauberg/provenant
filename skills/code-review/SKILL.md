---
name: code-review
description: "Use for independent read-only review of PRs, diffs, commits and refactors against the spec, through review-council or provenant review. Not for root cause (diagnose), fixes (implement) or rendered UI (ui-ux-design)."
---

# Code review

With little or no human diff review, an independent review panel plus CI on
the exact head is the check on the code. The panel must prove its coverage,
and "clean" must mean something.

Conventions and the review ladder come from the repo's own `AGENTS.md`,
`CONTRIBUTING` and runbooks. Look there first.

## How it runs

```sh
provenant run --preset review-council --prompt-file BRIEF.md \
  --set 'parameters.subject.commit="SHA"' --set 'parameters.base="origin/main"' \
  --set 'parameters.spec="#N"' --set 'parameters.risk_tags=["security"]' --plan on --explain
```

Lens coverage comes first: cover every question the change needs, and record
family separation as a bonus when available. The default is a planner-led panel
under the HARNESS review floors. The chair checks its recorded plan and supplies missing reviewers or lenses; today's
planner can only narrow the runtime-selected catalogue. The base lenses are
`correctness-spec`,
`design-fit-reuse`, `simplicity`, `conventions-readability`, `tests`,
`refactor-cleanup`, `reliability`, and `flexibility`; risk tags add
`security-privacy`, `concurrency-data-errors` and `adversarial`. Integration
heads get the full panel; the runtime may select `conventions-readability` alone
for docs, so add the policy-required coverage. A verifier tries to falsify
each finding and a senior synthesis gives each a disposition. For a quick fan-out without a graph:
`provenant review TARGET --brief-file B --shape quick|standard|deep`.
`deliver` already runs this council on the newest head.

For a native panel, take the lens list from `provenant review TARGET --explain`,
which prints the selected shape, lenses and the reason without dispatching. Launch
one independent read-only sub-agent per selected lens in parallel. Prefer the
cross-family leg the review ladder asks for, then have the chair synthesise
findings and dispositions across the panel.

Review a stacked PR against its own base; do not re-review lower PRs. See the [stacked PR workflow](../../docs/runbooks/github-workflow.md#stacked-pull-requests).

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

End with one final-line declaration using the
[review wire grammar](../../docs/specs/fabric.md#typed-node-outputs).
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
the lens prompts under `references/lenses/` load into the presets by path.

## Boundaries

`orchestrate` owns topology and scheduling; `diagnose` owns root cause;
`implement` owns repairs. For rendered-UI changes also inspect light and dark
captures of changed screens (`ui-ux-design`).
