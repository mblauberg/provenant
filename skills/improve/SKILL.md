---
name: improve
description: "Use to learn from runs and incidents, evaluate agents or prompts, or audit and write skills."
---

# Improve

Change process on evidence, choosing the best-supported remedy for the
observed cause. A clean routine cycle returns `no change`.

## Retrospect

Read available telemetry, review and repair history, escaped defects, PR threads,
human corrections and test-suite health. Mark absent evidence `unknown`, never
success inferred from a clean final answer. Use `provenant help telemetry` to
choose the cost, value and trace views. A suite that only grows is a finding.

Cluster failures by cause (spec, test, skill, instructions, routing, tool,
context, authority) and propose per cluster the best-evidenced change with its rationale,
owner, destination and success measure. Turn a representative failure into a
deterministic test or fixture before claiming improvement. `improved` needs a
passing regression gate and comparable later runs; underpowered or confounded
evidence is `inconclusive`. For comparisons across releases, experiments or
instructions in local Claude, Codex and lane histories, use the
[history audit](references/history-audit.md).

## Evaluate

For a stochastic system, prefer an execution oracle (hidden tests, type checks)
and grade with a model only what execution cannot establish, blind to the arm.
Freeze the task set, splits, criteria, arms, seeds and repetitions before
looking at results, and keep search, acceptance and held-out splits apart. For
a skill, compare the candidate with no skill and the previous version. Record
the actual model, harness, effort, cost and failures per arm, and report raw
numerators and denominators so another agent can recompute the result. A result
describes its tested distribution, not general safety.

## Skills

A skill earns its place with the few rules a capable agent would otherwise get
wrong; everything else is cost paid on every load. Audit read-only with
[skill audit](references/skill-audit.md); write or revise under a grant with
[skill author](references/skill-author.md). Run the task without the skill
first and keep only the lines that change the result.

## Output

Proposal-first. Apply changes under explicit authority or through `deliver`.
Finish with a table: finding, evidence, cause, change, regression gate,
destination, status (`promote`, `experiment`, `defer`, `reject`). File
actionable findings through `tracker` even when no regression is attributable,
and promote durable conclusions to their owner rather than a parallel diary.
