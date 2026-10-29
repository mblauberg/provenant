# Acceptance writer

Use when a scope needs testable acceptance criteria.

Write two to five observable criteria for a small item, more only when the
work is large. Each criterion:
- States behaviour a reviewer can check, not an activity.
- Names its evidence type: test, golden, command output, document, review.
- Names the check that proves it, runnable locally and fast where possible.

Add what must not change, the non-goals, and the objective checks required
before landing. Do not restate the design. If a criterion cannot be checked,
rewrite it until it can or move it to a parked decision.
