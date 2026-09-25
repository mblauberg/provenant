---
name: tdd
description: "Use for writing the first right-reason failing test for new or changed observable behaviour, then red-green-refactor. Not for behaviour-preserving structure, diagnosis-only, or delivery; use refactor, diagnose, or implement."
---

# Test-driven development

```text
NO NEW OR CHANGED OBSERVABLE BEHAVIOUR WITHOUT A RIGHT-REASON FAILURE FIRST
```

Observable behaviour is what a user or caller can depend on: an outcome, a
returned value, a stored record, a public contract. Every test is code someone
must maintain, so it must catch a plausible defect that no existing test
catches. See [When not to write a test](#when-not-to-write-a-test).

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
   immediately either needs sharpening or shows the behaviour already exists;
   decide which before writing production code.
2. **Green:** write only the simplest production change that passes. Avoid
   speculative options and unrelated cleanup. Run the focused test each cycle,
   affected checks at tranche boundaries, and the full required suite at the
   enclosing verification gate; output must be clean.
3. **Refactor:** only while green, remove duplication, improve names and deepen
   modules, in tests as well as production code: merge near-duplicate tests and
   delete ones a stronger test subsumes. Rerun tests after each structural step.

When project instructions set a later execution point for tests, such as a
merged-tree gate under a machine budget, follow that cadence: still write each
test first, name the mutation or missing behaviour it must fail on, and report
its red and green runs as deferred. That gate must run them before merge; a
deferred run is not yet evidence.

Work vertically: test -> implementation -> repeat. Start with one tracer bullet
through the full path, then add behaviours from what each cycle reveals. Never
write an imagined horizontal test batch first. Prioritise critical paths and
complex logic.

## Test boundaries

Test what callers observe through the public interface, not private methods,
internal call order, internal data shape or side channels. Wire formats, public
result shapes and persisted records are contracts. A valid test survives an
internal refactor and a restyle. [tests.md](references/tests.md) lists what a
test may and may not pin.

**Prose is not a contract.** Documentation, skill and ADR wording, and
configuration literals nothing consumes are not behaviour and earn no test. Name the production
change that would make a test fail before writing it; if the only answer is
"someone reworded the text", write none. Assert parsed structure or a script's
effects, never the source text.

Mock only system boundaries: external APIs, time/randomness and, when needed,
filesystem/database. Never mock owned internal collaborators to pass a test;
inject a narrow boundary instead. Assert a mock's calls only when the call is
itself the contract, such as a charge or a sent message. See [mocking.md](references/mocking.md)
and [interface-design.md](references/interface-design.md).

Hard-to-test code is design evidence: accept dependencies, return results
instead of hiding effects, and prefer deep modules with small interfaces. On
green, use [deep-modules.md](references/deep-modules.md) and
[refactoring.md](references/refactoring.md); do not smuggle redesign into green.

## When not to write a test

Write no test, and give this evidence instead, for:

- **Prose, copy and unconsumed configuration:** the diff and review.
- **Appearance, layout and motion:** a rendered capture or recording, judged by
  a person or a visual diff; never class, style or DOM-structure assertions.
- **A behaviour-preserving refactor:** the existing suite staying green.
- **What types, a schema, a parser or a linter already guarantee:** that check.
- **Trivial delegation or wiring with no decision in it:** the caller's test.
- **A case an existing test already covers, here or at a lower layer:** that
  test. Test a shared unit once where it is owned, not again through each
  consumer.
- **An exploratory probe or debugging script:** nothing; delete it before
  handoff.
- **A test for which no plausible defect comes to mind:** nothing.

## Existing tests

Agents copy the tests beside them. In a file you touch, rewrite or delete a
rigid test on the behaviour you are changing rather than adding another like
it. When a behaviour-preserving change fails a test, the test pinned an
incidental detail: rewrite it to pin behaviour or delete it; never bend
production code around it. Never weaken, skip or delete a test to make a real
behaviour change pass, and report every test you remove with its reason. A
characterisation test that pins current, possibly wrong behaviour is temporary:
replace it with an intended-behaviour test once the change lands.

## Where a test lives

Put the test in the file that already owns the unit's behaviour, following this
repository's layout and naming. A new test file needs a new production unit with
no owning file; a review, audit or documentation pass never justifies one.

## Advance gate

Before the next behaviour confirm: public observable contract; a named defect
no existing test catches; nothing incidental pinned; witnessed right-reason
failure; minimal passing code; focused and broader checks green;
no dirty output; resilience to internal refactoring. Under a project-deferred
cadence, report each unwitnessed item as deferred to the gate.

For a bug, reproduce the failing condition as the regression test, asserting
the corrected behaviour rather than today's output or error text; watch it
fail, then repair and watch it pass. If no correct seam reaches the bug, route
to `diagnose`; do not bless a shallow test. `implement` owns independent review,
repair loops, documentation and user acceptance.
