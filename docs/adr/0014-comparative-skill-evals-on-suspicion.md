# ADR 0014: Comparative skill evals on behaviour change or suspicion

**Status:** Accepted

Changed: 2026-10-26, #1754, #1761.

## Context

Structural fixtures catch malformed routes, but cannot show whether a skill
improves the agent's choices. Full frozen comparative studies cost more than
routine maintenance in a personal harness.

## Decision

Every skill change runs its trigger fixtures. Contract tests protect only
machine-enforced invariants; prose alone does not require a contract test.

A skill behaviour change triggers a run of its fixtures with and without the
skill. Record the difference, including regressions, omissions and failures,
for the reviewer. Scores are advisory and do not fail CI.

Frozen held-out comparative evals remain conditional. Run them when a routing
regression is suspected or observed, when preparing publication or use by
another operator, or at the maintainer's discretion when rewriting
trigger-bearing descriptions across several skills. Compare candidate,
without-skill and previous-package arms on current primary families. Retain
invalid, omitted, timed-out and failed attempts with model lineage and cost.

## Consequences

Behaviour changes always supply comparative evidence. Wording-only maintenance
keeps the cheap fixture checks without requiring a full held-out study.
Detection in ordinary use remains the routing-regression backstop between
studies. Publication re-arms the full comparison requirement.

This process record is retained as the explicit in-place amendment required by
[#1761](https://github.com/mblauberg/provenant/issues/1761); new process rules
belong in their governing skill.
