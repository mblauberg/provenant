---
name: code-review
description: "Use for independent read-only review of a pinned diff or pull request against its spec."
---

# Code review

Review independently at a pinned head and prove coverage alongside CI.

The chair or lead chooses whether to review, the reviewers and their number.
For routine, low-risk work, use one reviewer or none. For risky changes, prefer
a strong reviewer from another model family, optionally paired with one from
the author's family. Avoid a lightweight cross-family slot; if only the same
family is available, warn and proceed. These are overridable preferences, not
routing requirements. Sources: [review diversity](https://arxiv.org/abs/2610.01471),
[family bias](https://arxiv.org/abs/2609.17857) and
[review trade-offs](https://marmelab.com/blog/2026/09/24/the-state-of-ai-harness-engineering-2026.html).

For a configured panel, pin the subject and base and supply the spec:

```sh
provenant run review-panel --brief-file brief.md --set subject=origin/branch --set review_base=origin/main
```

A dotted leaf such as `review.security` selects one lens. Review a changed
subject against prior findings and the invariant under repair.

## Independence

- Follow the pinned-commit and read-only rules in the reviewer context.
- The brief carries the spec, never the author's hypothesis, expected verdict
  or scratchpad.
- A review that cannot read its rubric or required context is `incomplete`,
  never `clean`.

## Rubric

[reviewer-context.md](references/reviewer-context.md) defines findings;
[review-lenses.md](references/review-lenses.md) supplies inspection questions.
Template briefs live under `references/lenses/`.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, persisted state), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that criteria map
to evidence and that no test was loosened or deleted to mask a real defect
(pruning rigid, redundant or implementation-pinned tests with stated reasons is
welcome). For rendered UI, use `ui-ux-design` to choose representative states.
