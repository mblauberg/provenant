---
name: tdd
description: "Use for writing the first right-reason failing test for new or changed observable behaviour, then red-green-refactor. Not for behaviour-preserving structure, diagnosis-only, or delivery; use refactor, diagnose, or implement."
---

# Test-driven development

NO NEW OR CHANGED OBSERVABLE BEHAVIOUR WITHOUT A RIGHT-REASON FAILURE FIRST.

Observable behaviour includes outcomes, returned values, stored records and
public contracts. Appearance, layout and motion need rendered evidence.

## Red -> green -> refactor

1. Write one public-interface test for one behaviour. Witness failure because
   behaviour is missing, not setup, import or typo. If it passes immediately,
   sharpen it or establish that the behaviour already exists.
2. Make the simplest passing change. Work vertically, test then implementation;
   never write an imagined horizontal batch. Run focused tests each cycle,
   affected checks at tranche boundaries and the full required suite at the
   enclosing verification gate.
3. Refactor only while green, including tests. Remove duplication and rerun
   checks; never smuggle redesign into green.

Read [cycle details](references/cycle.md) for deferred project cadence,
tracer bullets and the optional independent acceptance-test writer for crucial work.
Deferred red or green runs remain unverified until their gate runs before merge.

## Hard boundaries

- Never delete or overwrite unknown, pre-existing or user-authored work to
  create red. Preserve any production patch this run wrote prematurely;
  remove only its authorised hunks, witness red, then reapply minimally.
  Existing code needs a regression or characterisation seam before repair.
- Throwaway prototypes, generated code and configuration need an explicitly
  recorded exception; deadline pressure grants none.
- Test public behaviour, not private methods, call order, internal shape,
  source text, prose, classes, styles or DOM structure. Wire formats,
  public result shapes and persisted records are contracts. Required disclosures,
  accessible names and public messages keep their contract tests.
- Name the plausible production defect no existing test catches. Write no
  test for duplication, trivial wiring, unconsumed configuration or guarantees
  already enforced by types, schemas or lint; give the named existing check.
  Money, authorisation, privacy and data integrity always earn tests.
- Expected values come from the spec, never implementation output. Assert
  specific error variants or codes. Stop on a spec/test conflict.
- Never weaken, skip or delete tests to pass a real behaviour change:
  no specific-error-to-`is_err()`, exact-to-at-least count, derived expectations
  or test-input special cases. Report removed tests and reasons.
- For a preserving change, establish the public contract before replacing
  an incidental assertion. Replace temporary characterisation with intended
  behaviour after repair.
- Mock only external APIs, time/randomness and necessary filesystem/database
  boundaries; never owned collaborators. Calls are assertions only when contractual.
- Use the existing owning test file. A new file requires a new production
  unit without an owner; an audit or documentation pass grants none.

Read [test forms, exclusions, existing-test repair and advance gate](references/tests.md)
before choosing evidence. Use [mocking](references/mocking.md) and
[interface design](references/interface-design.md) for boundary problems;
[deep modules](references/deep-modules.md) and
[refactoring](references/refactoring.md) while green.
If no correct seam reaches a bug, use diagnose. Implement owns independent
review, bounded repair and delivery acceptance.

Reference eval cases (not run by CI): [discipline cases](evals/discipline_cases.yaml).
