---
name: evaluate
description: "Use for repeatable evaluation of agents, prompts, rankings or other judgement-bearing systems. Not for deterministic tests or ordinary source review; use test-design or code-review."
---

# Evaluate

Choose an execution oracle first, such as hidden tests or type checks. Use a model grader only for criteria that execution cannot establish; blind it to the treatment and retain criterion evidence and disagreements.

Freeze the decision, task set and split digests, provenance, acceptance criteria, comparator arms, paired seeds and repetitions before observing results. Keep search, acceptance and held-out splits separate. For skills, compare the candidate, no-skill baseline and previous version. Record actual model, harness, effort, cost and failures for each arm.

Use the project's frozen protocols in `docs/evals/` where applicable. `provenant bench mine` prepares benchmark tasks, `provenant bench verify` checks a frozen task, and `provenant bench run` runs a split. Consult the bench runbook for inputs and leakage controls. Retain the frozen protocol, task data, attempts and result rows, including errors, omissions and retries. Report raw numerators and denominators, failure clusters and uncertainty; another agent must be able to recompute the result from those artefacts.

Compare against thresholds set in the frozen protocol, not an invented universal margin. Telemetry experiment comparisons are advisory. A result describes its tested distribution, not general safety or delivery acceptance. For stochastic searches report the best found within the sampled envelope, not impossibility. Route defects to implement and unsettled requirements to scope; use deliver for the outcome report.
