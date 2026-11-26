---
name: deliver
description: "Use to build and land approved work: briefs, CI, review, repair, refactors and the outcome report."
---

# Deliver

Start from an approved spec: outcome, criteria with how each is verified, risk
tier and write authority. A missing piece goes back to `scope`; an engineering
call is recorded as `Decided (<source>): <reason>` and work proceeds.

The `deliver` template runs a writer through CI, review, repair and landing;
`implement` is one slice, `implement.fix` a known defect and `batch` several
slices into one integration branch. `orchestrate` holds the command shapes.
Apply the [engineering stance](references/engineering.md) to design, review and
tests.

## The brief

Give the writer the outcome, criteria, known owners and checks. Name the method:
`test-design` for behaviour, `diagnose` for an unknown cause, or the refactor
rules below for restructuring.

## Writer rules

- Never weaken an assertion to pass. Triage a failing test as `test-design`
  describes and report material test changes with their reason.
- Update the owned docs in the same pull request; a settled decision moves to
  its durable home (`engineering-docs`).
- Move every caller and delete the old shape without compatibility code.
  Persisted state follows the owning repository's format policy.
- When the spec is wrong, cite the evidence in the declaration `decisions` and take
  the best-evidenced option within write scope, recording why it beat the
  alternative. If no option serves the outcome, return the evidence to `scope`.

## Refactors

Pick the oracle before changing structure: the compiler for type-driven moves,
a reviewed codemod for uniform changes across many sites, a differential run or
characterisation tests. Record the baseline so another agent can repeat it, use
the same oracle across the change, investigate unexplained behaviour changes
and report the ownership and dependency delta.

## Repairing a red pull request

Give the repair writer the output of `provenant ci failures pr N` (several
numbers at once for several PRs) and `provenant pr status N`. Raw job-log reads
spend the forge quota and miss the failure summary. When the REST quota is
exhausted, read checks and releases through `gh api graphql`, which has a
separate quota.

## Judging review findings

When the chair runs review and repair by hand, judge each finding against the
code before a writer starts: a lens can be wrong and a writer patches what it
is given. Keep rejected findings, with the reason, out of the repair brief.
Group confirmed findings by cause and state the invariant under repair, so one
flaw is fixed once and its twin elsewhere is caught. The chair may overrule a
changes verdict with the reason on the pull request; a dispute at a real
boundary or over data loss needs a test or a second lens. After a shared flaw
or a third round, ask whether the design is wrong.

## Work without CI

- **Research:** `research` gathers sourced claims, each with a source, a date
  and whether it was observed or inferred; `synthesise` reconciles them.
- **Documents:** a read-only `verify` against the sources and criteria lists
  unsupported claims, not stylistic edits.
- **Agent output:** deterministic evidence (a diff, a recomputation, a test)
  before judgement. Stochastic behaviour goes to `improve`.

## Done

Exact-head CI and the review the tier requires meet acceptance. Write the
report for a reader who will not open the artefact. Lead with any decision
needed, then:

1. What changed or was produced, and where it lives.
2. Each criterion with its evidence, or `unverified` and why.
3. Review coverage: who reviewed, through which lenses, and what was not
   covered.
4. Assumptions made on the owner's behalf.
5. Follow-ups, each as a tracker issue or a named owner.

Promotion needs its own authority (`release`).
