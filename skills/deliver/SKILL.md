---
name: deliver
description: "Use to build and land approved work: briefs, CI, review, repair, refactors and the outcome report."
---

# Deliver

Start from an approved spec: outcome, criteria with how each is verified, risk
tier and write authority. A missing piece goes back to `scope`; an engineering
call is recorded as `Decided (<source>): <reason>` and work proceeds.

The `deliver` template runs a writer through CI, review, repair and landing;
`implement` is one slice, `implement.fix` a known defect and `batch` disjoint
slices into one integration branch. `orchestrate` holds the command shapes.

## The brief

The writer sees only the brief: the issue, the criteria, the files and owners
already found, the checks to run, and an instruction to stop and report rather
than widen scope. Name the method: `test-design` for behaviour, `diagnose` for
an unknown cause, the refactor rules below for restructuring.

## Writer rules

- Never weaken an assertion to pass. When an existing test fails, decide
  whether it caught a defect or pins implementation detail: fix the code for a
  defect; rewrite the test as an invariant or delete it if it pins
  implementation. Keep regression tests minimal and prune redundant tests.
  Report every changed or removed test with its reason.
- Update the owned docs in the same pull request; a settled decision moves to
  its durable home (`engineering-docs`).
- Keep compatibility only for published contracts. Otherwise change the
  callers and delete the old path; persisted state changes ship a migration.
- Report checks that did not run as unverified, never as passing.
- When the spec is wrong, cite the evidence in the handoff `decisions` and take
  the smallest option inside write scope that serves the outcome. If none
  exists, return the evidence to `scope`.

## Refactors

Pick the oracle before changing structure: the compiler for type-driven moves,
a reviewed codemod for uniform changes across many sites, a differential run or
characterisation tests. Record the baseline so another agent can repeat it.
Land a shared seam first, then the large change under the same oracle with
disjoint write scopes. Delete the old path in the same change; no shims, legacy
readers or fallbacks. A temporary strangler is for live traffic or persisted
data on the move, with an expiry. Stop on an unexplained oracle failure or a
discovered behaviour change. Report the ownership and dependency delta.

## Judging review findings

When the chair runs review and repair by hand, judge each finding against the
code before any writer starts: a lens can be wrong and a writer patches what it
is given. Keep rejected findings, with the reason, out of the repair brief.
Group confirmed findings by cause so one flaw is fixed once, and state the
invariant under repair so a twin copy elsewhere is caught. The chair may
overrule a changes verdict with the reason on the pull request; a dispute at a
real boundary or over data loss needs a test or a second lens. After a shared
flaw or a third round, ask whether the design is wrong; the round count prompts
that judgement and does not end the work.

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
