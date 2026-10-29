# Acceptance writer

Use when a scope needs testable acceptance criteria.

Write 2 to 5 observable criteria for a task and about 10 for a feature; more means
an epic. Each criterion:
- is one statement of behaviour a reviewer can check, `When <event>, <behaviour>`;
- names its evidence type: `[test]`, `[command]`, `[capture]` or `[review]`;
- names the check that proves it, runnable locally and fast where possible.

Add must-not-change behaviour, non-goals and the verify command. Do not restate the
design. If a criterion cannot be checked, rewrite it until it can, or park it as a
decision. The criteria are the contract: the implementer may not weaken or delete
one, and a risky item keeps one held-back check outside the writer's context.
