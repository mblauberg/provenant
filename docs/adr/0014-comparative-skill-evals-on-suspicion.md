# ADR 0014: Comparative skill evals run on suspicion, not by default

## Status

Accepted (wording user-approved 2026-07-20). Lands with PR 4 of the
[disclosure-refactor spec](../specs/harness/disclosure-refactor.md), which
also amends `MAINTAINING.md`.

Amended 2026-09-02 (issue #761): the contract-test half of the first Decision
bullet is narrowed. Trigger fixtures remain mandatory for every skill change;
a contract test is required only for an invariant a script, validator or CI
gate evaluates at runtime. `MAINTAINING.md` §"Change a skill" step 7 is the
live statement of the rule.

## Context

`MAINTAINING.md` requires frozen held-out eval comparisons (candidate vs
without-skill vs previous package, across primary families) for every material
skill change. The requirement guards against silent trigger-routing
regressions: a reworded description can stop a skill firing, or steal a
sibling's triggers, and no structural or contract test observes model routing
behaviour.

That guard is priced for a published, multi-user harness. This repository
currently serves one operator who uses the skills daily; a routing regression
surfaces within a day of normal use, and the eval machinery (frozen cases,
repeated trials, per-family lineage records) costs more attention than the
failures it would catch. The cheap layer — positive/negative/boundary trigger
fixtures and machine-enforced contract tests — catches structural drift at
near-zero cost and stays.

## Decision

- Trigger fixtures and contract tests remain mandatory for every skill change.
- Frozen held-out comparative evals become conditional. Run them when:
  1. a routing regression is suspected or observed in use;
  2. a change rewrites trigger-bearing description text across several skills
     at once (as in the disclosure refactor), at the maintainer's discretion;
  3. the harness is being prepared for publication or another operator.
- The eval machinery (`evals/`, held-out datasets, lineage recording rules) is
  retained, not deleted, so the conditional path stays cheap to invoke.

## Consequences

- Routine skill maintenance loses a heavyweight gate; the operator accepts
  detection-in-use as the routing-regression backstop.
- Publication to other users re-arms the full requirement; this ADR must be
  revisited in any public-release checklist.
- `MAINTAINING.md` §"Change a skill" and §"Change the delivery kernel" are
  amended to state the conditional rule and cite this ADR.
