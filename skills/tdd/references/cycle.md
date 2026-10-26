# Test cycle

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

For crucial work, optionally have a fresh-context agent write acceptance tests
from the approved spec and public interface, without implementation code,
observed outputs or the author's proposed assertions. Keep the same vertical
cycle: agree the oracle and witness each right-reason failure before its
implementation. The author must not loosen those tests to reach green.
