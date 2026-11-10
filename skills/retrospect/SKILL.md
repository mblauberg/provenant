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
  attempts and retries; `provenant telemetry cost --by route` for spend; `provenant telemetry trace` for one run's path;
  `provenant telemetry experiments show NAME` for arm comparisons.
- `provenant quota usage` and `provenant quota history` for quota against
  allowance; `provenant telemetry value` for plan value.
- Review and repair history, escaped defects, the PR threads and any human
  correction of an unattended run. A correction names a class of defect: sweep
  for the class.
- Test-suite health: tests added against removed, the mock ratio and
  assertion changes. Use the target repository's test-diff tooling where its
  `AGENTS.md`, `CONTRIBUTING` or runbooks name it; otherwise compare the test
  diff by hand. A suite that only grows is a finding.

For comparisons across experiments, releases or instructions, use the
[local history audit](references/history-audit.md), including Claude, Codex and
lane histories. Keep recorded exposure separate from historical proxies.
Report non-identifiability plainly; file actionable findings through `tracker`
even when no regression can be attributed. Raw history may go to other
providers only when the owner authorised that run; exclude credentials and
secrets. Local read-only analysis needs no receipt.

For a wide review, fan out readers with `provenant run --use explore --route junior --over costs,quality,recovery --file Q.md`, one slice of
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

Once a quarter, refresh the research behind the skills against current primary
sources and mark any older than a year as possibly stale.

## Output

Proposal-first and read-only. Apply changes only under explicit authority or
inside an `implement` run. Finish with a table: finding, evidence, cause,
change, regression gate, destination, status (`promote`, `experiment`,
`defer`, `reject`). Promote durable conclusions to their owner (spec, ADR,
runbook, instructions, test, skill) and merge with existing text; never keep a
parallel diary.
