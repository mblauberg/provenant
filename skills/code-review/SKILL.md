---
name: code-review
description: "Use for independent read-only review of a pinned diff or pull request against its spec."
---

# Code review

Review independently at a pinned head and prove coverage alongside CI.

Run the `review` template with the subject and base on the command line (the
command is in `orchestrate`; the host computes the merge base and supplies
pinned Git context) and the spec in the brief. A dotted flavour such as
`review.security` selects one lens; the composite resolves a panel at launch,
then `review.verify` and `review.synthesis` where needed. Review each slice
lightly and the integrated head with the full required panel. A changed subject
gets a delta review against the prior findings and the invariant under repair.

## Independence

- The reviewer reads the pinned commit with `git show` and `git diff`, never
  the working tree, and uses no Git write verbs.
- The brief carries the spec, never the author's hypothesis, expected verdict
  or scratchpad.
- A review that cannot read its rubric or required context is `incomplete`,
  never `clean`.

## Rubric

[reviewer-context.md](references/reviewer-context.md) defines blocking,
should-fix and note, what a finding states and what not to report. Every lens
receives it, and a manual review applies it too. Inspection questions are in
[review-lenses.md](references/review-lenses.md); the prompts under
`references/lenses/` supply the template briefs.

## What a clean verdict proves

Say what was inspected: the dependency cone read (callers, owners, tests,
config, migrations), the checks that ran with their results, and what was not
reachable. Never infer coverage from a green summary. Check that criteria map
to evidence and that no test was loosened, skipped or deleted to get a green
run. For rendered UI, also inspect light and dark captures of changed screens
(`ui-ux-design`).
