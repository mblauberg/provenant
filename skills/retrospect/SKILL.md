---
name: retrospect
description: "Use after delivery, release, incidents or long unattended runs to turn evidence into process changes and regression gates. Not for context cleanup (session), skill audits (skill-craft) or active defects (diagnose, implement)."
---

# Retrospect

Change the process only on evidence, and only by the smallest change that
would have prevented the observed cost. A clean routine cycle returns
`no change`. One dated log per run is clutter, not a retrospective.

## Evidence

Read what exists, and mark what is absent `unknown` rather than inferring
success from a clean final answer:

- `provenant telemetry stats --by preset` (or `pr`, `issue`, `role`) for
  attempts, cost and retries; `provenant telemetry trace` for one run's path;
  `provenant telemetry experiments show NAME` for arm comparisons.
- `provenant quota usage` and `provenant quota history` for spend against
  allowance.
- Review and repair history, escaped defects, the PR threads and any human
  correction of an unattended run. A correction names a class of defect: sweep
  for the class.
- Test-suite health: tests added against removed, the mock ratio and
  assertion changes. Use the target repository's test-diff tooling where its
  `AGENTS.md`, `CONTRIBUTING` or runbooks name it; otherwise compare the test
  diff by hand. A suite that only grows is a finding.

For a wide review, fan out readers with `provenant run --preset
fan-out-synthesise --set 'parameters.width=3' --prompt-file Q.md`, one slice of
runs per branch.

## Method

1. Compare the outcome with the declared criteria and with a comparable run.
2. Cluster recurring failures by cause: spec, test, skill, instructions,
   routing, tool, context or authority.
3. Propose per cluster the smallest change, with evidence, owner, risk,
   destination and a success measure.
4. Turn a representative failure into a deterministic test or a fixture before
   claiming improvement.
5. Watch the next comparable cycle for recurrence and new costs. `improved`
   needs a passing regression gate and enough comparable later runs;
   underpowered or confounded evidence is `inconclusive`.

Once a quarter, refresh the research behind the skills against 2026 primary
sources and mark older ones as possibly stale.

## Output

Proposal-first and read-only. Apply changes only under explicit authority or
inside an `implement` run. Finish with a table: finding, evidence, cause,
change, regression gate, destination, status (`promote`, `experiment`,
`defer`, `reject`). Promote durable conclusions to their owner (spec, ADR,
runbook, instructions, test, skill) and merge with existing text; never keep a
parallel diary.
