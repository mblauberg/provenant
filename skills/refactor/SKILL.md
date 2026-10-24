---
name: refactor
description: "Use for authorised, behaviour-preserving structural simplification backed by characterisation and equivalence evidence. Not for new behaviour, read-only review, or unknown root cause; use tdd, code-review, or diagnose."
---

# Refactor

Execute a structural method inside an authorised `implement` run. This skill
inherits exact paths, non-goals and repair limits; it cannot widen scope, accept
the change, release it or turn nearby cleanup into work.

## Entry evidence

Before editing, record:

- the named cost/risk to remove and the observable behaviours that must not
  change;
- public API, schema, protocol, data, ordering, error and performance invariants;
- a green baseline plus characterisation, differential, replay, property or
  compatibility checks selected by risk;
- the dependency cone: callers, exports, registration/configuration, generated
  artifacts, persistence and mixed-version boundaries;
- canonical ownership, duplicate/parallel paths, rollback seam and a
  move/deletion manifest.

Use SOLID and related principles as probes, not success metrics: improve
cohesion and single ownership; preserve substitutability and caller contracts;
segregate interfaces only where clients carry unused coupling; invert volatile
dependencies only when the new seam hides real complexity. Prefer simple,
explicit state and information hiding over speculative abstraction, pattern
count, DRY-by-text or acronym compliance.

If the desired structure or behaviour is undecided, route to `scope`. If a
failure's cause is unknown, use `diagnose`. Changed behaviour is a separate TDD
slice, never hidden in the refactor.

## Method

Use structural search when code shape matters more than spelling. `ast-grep`
can locate syntax patterns across formatting and identifier changes; for
example, `sg -p '$A.unwrap()'` finds unwrap calls and
`sg -p 'Command::new($BIN)'` finds process construction sites.

1. Establish a seam while green. Do not start by deleting the old path.
2. Move one independently reviewable responsibility at a time. Preserve the
   public contract or use an approved compatibility adapter with an expiry.
3. Switch callers/registrations to one canonical owner and verify after each
   slice. Keep focused checks fast; run affected and full required gates at the
   enclosing tranche.
4. Delete only when caller, runtime, build, registry, reflection/dynamic-load,
   configuration and data evidence show the path is unreachable or replaced.
   Unknown, user-owned and unrelated files stay untouched.
5. Stop on behaviour drift, unexplained performance/resource change, scope
   expansion or an invariant the evidence cannot cover. Revert only exact
   run-owned hunks under authority and return to `scope`/`diagnose` as needed.

## Completion gate

Provide before/after ownership and dependency deltas, the classified
move/deletion manifest, exact verification results and residual risk. Pass only
when observable/API/schema/data contracts remain equivalent, the named
structural debt is measurably reduced, no unowned parallel path remains,
relevant performance/resource budgets hold, documentation is current and an
independent `code-review` finds no blocking regression. Lines deleted alone are
never proof.
