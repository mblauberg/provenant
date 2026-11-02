## Changes
### Review verdicts

- Reviews now use the shared final-line [verdict grammar](../../specs/fabric.md#typed-node-outputs). Duplicate declarations are unknown. Retained legacy first-line review outputs remain readable for one release when they contain exactly one declaration.
- Verdict gates compare canonical review tokens. Existing preset and role aliases remain compatible, including a gate consuming another gate's output. Stored checkpoint verdict strings are unchanged.
- Graph run summary verdict keys now use canonical review tokens. Review JSON can report `approve_with_nits`; `provenant review` still succeeds only when every reviewer succeeds and approves.
- Workflow review coverage accepts `inconclusive` as non-passing and non-blocking, with a diagnostic requesting a fresh review.
