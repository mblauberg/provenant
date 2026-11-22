---
name: code-review
description: "Use for independent read-only review of a pinned diff or pull request against its spec."
---

# Code review

Review independently at a pinned head and prove coverage alongside CI.

The chair or lead chooses whether to review, the reviewers and their number;
`provenant workflow review` checks recorded coverage against the receipt's risk
tier. A small change needs no review and routine work takes one reviewer. For
risky changes, cover the lenses the risk requires first; a strong reviewer from
another model family is a bonus, optionally paired with one from the author's
family. If only the same family is available, warn and proceed. These are
overridable preferences, not routing requirements.

For a configured panel, pin the subject and base and supply the spec:

```sh
provenant run review-panel --pr 123 --brief-file brief.md
```

A dotted leaf such as `review.correctness` selects one lens. The security-privacy
lens is selected by credential, secret or publication paths, or when the owner
asks. Review a changed subject against prior findings and the invariant under
repair.

## Independence

- Follow the pinned-commit and read-only rules in the reviewer context.
- The brief carries the spec, never the author's hypothesis, expected verdict
  or scratchpad.
- A review that cannot read its rubric or required context is `inconclusive`,
  never `clean`.

## Rubric

[reviewer-context.md](references/reviewer-context.md) defines findings and
blocking; [review-lenses.md](references/review-lenses.md) supplies the shared
inspection questions. The lens briefs sit under `references/lenses/`.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, persisted state), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that criteria map
to evidence and that no test was loosened or deleted to mask a real defect
(pruning rigid, redundant or implementation-pinned tests with stated reasons is
welcome). For rendered UI, use `ui-ux-design` to choose representative states.
