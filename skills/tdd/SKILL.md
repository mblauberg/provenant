---
name: tdd
description: "Use for writing the first right-reason failing test for new or changed observable behaviour, then red-green-refactor. Not for behaviour-preserving structure, diagnosis-only, or delivery; use refactor, diagnose, or implement."
---

# Test-driven development

```text
NO NEW OR CHANGED OBSERVABLE BEHAVIOUR WITHOUT A RIGHT-REASON FAILURE FIRST
```

Never delete or overwrite unknown, pre-existing or user-authored work to create
a red state. If this run wrote production code before the test, preserve its
patch, remove only the exact run-owned hunks when authorised, witness the
right-reason failure, then reapply and minimise them. Existing code gets a
regression or characterisation seam before repair. Deliberate throwaway
prototypes, generated code and configuration may use an explicitly recorded
exception; deadline pressure is not one.

## Red -> green -> refactor

1. **Red:** write one test for one observable behaviour through the public
   interface. Name it like a specification. Run it and confirm it fails because
   the behaviour is missing, not from setup, import or typo. A test that passes
   immediately needs sharpening.
2. **Green:** write only the simplest production change that passes. Avoid
   speculative options and unrelated cleanup. Run the focused test each cycle,
   affected checks at tranche boundaries, and the full required suite at the
   enclosing verification gate; output must be clean.
3. **Refactor:** only while green, remove duplication, improve names and deepen
   modules. Rerun tests after each structural step.

Work vertically: test -> implementation -> repeat. Start with one tracer bullet
through the full path, then add behaviours from what each cycle reveals. Never
write an imagined horizontal test batch first. Prioritise critical paths and
complex logic.

## Test boundaries

Test what callers observe through the public interface, not private methods,
internal call order, data shape or side channels. A valid test survives an
internal refactor. See [tests.md](references/tests.md).

**Prose is not a contract.** Documentation, skill and ADR wording, and
configuration literals are not behaviour and earn no test. Name the production
change that would make a test fail before writing it; if the only answer is
"someone reworded the text", write none. Assert parsed structure or a script's
effects, never the source text.

Mock only system boundaries: external APIs, time/randomness and, when needed,
filesystem/database. Never mock owned internal collaborators to pass a test;
inject a narrow boundary instead. See [mocking.md](references/mocking.md)
and [interface-design.md](references/interface-design.md).

Hard-to-test code is design evidence: accept dependencies, return results
instead of hiding effects, and prefer deep modules with small interfaces. On
green, use [deep-modules.md](references/deep-modules.md) and
[refactoring.md](references/refactoring.md); do not smuggle redesign into green.

## Where a test lives

Put the test in the file that already owns the unit's behaviour, following this
repository's layout and naming. A new test file needs a new production unit with
no owning file; a review, audit or documentation pass never justifies one.

## Advance gate

Before the next behaviour confirm: public observable contract; witnessed
right-reason failure; minimal passing code; focused and broader checks green;
no dirty output; resilience to internal refactoring.

For a bug, reproduce the exact failure as the regression test, watch it fail,
then repair and watch it pass. If no correct seam reaches the bug, route
to `diagnose`; do not bless a shallow test. `implement` owns independent review,
repair loops, documentation and user acceptance.
