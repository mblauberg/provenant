---
name: evaluate
description: "Use for repeatable quality or safety evaluation of stochastic or judgement-bearing systems, prompts, agents, rankings, or artifacts. Not for deterministic tests or ordinary code review; use tdd or code-review."
---

# Evaluate

Turn judgement into a frozen, repeatable assurance result. Deterministic
tests still run; this skill covers behaviour whose quality cannot be proven by
an exit code alone.

## Contract

Freeze the canonical evaluation plan before seeing results:

- decision the evaluation informs and unacceptable failure modes;
- hash-bound dataset/corpus, provenance, consent or licence, data policy and
  holdout boundary;
- candidate and applicable comparator manifests, paired sampling, seeds,
  repetitions, timeouts, retries and exclusions;
- metric ranges, aggregation, thresholds and per-metric regression margins;
- deterministic preflight, rubric, blinded independent graders and
  disagreement handling;
- safety, bias, privacy and adversarial cases proportionate to risk;
- enclosing delivery run when this evidence supports a non-trivial outcome.

Use [EVALUATION.template.json](templates/EVALUATION.template.json) and the
[receipt contract](references/receipt.md). Keep raw examples outside the hot
receipt; link safe relative artifacts and SHA-256 digests.

## Run

1. Validate fixtures, hashes, schemas and leakage controls before any judgement
   attempt. Deterministic failure stops or explicitly skips the frozen schedule.
2. Run each planned arm/family/repetition. Retain every attempt, retry and case
   row with actual adapter/provider/model/effort lineage and usage disposition.
3. Account for passes, failures, omissions, skips, exclusions, timeouts, invalid
   output and tool/provider errors. A retry appends; it never erases its parent.
4. Aggregate from retained rows. Report raw numerators/denominators,
   distributions and failure clusters, rather than averages alone. Schema v2
   supports mean aggregation only; distributions and tail analysis live in
   linked digest-bound evidence until another aggregation is added through a
   new schema version.
5. Blind independent graders to treatment identity. Record criterion evidence;
   send disagreements to a fresh adjudicator rather than majority vote.
6. Compare with frozen thresholds and applicable arms. A post-hoc change creates
   a fresh run; it does not rewrite the completed evidence.
7. Preserve bounded failures and route product defects to `implement`, unclear
   requirements to `scope`, and operational regressions to `diagnose`.

Before you call a receipt final, read it against this checklist:

- `plan.digest` still matches the plan frozen before results were seen, and
  `plan.frozen_at` precedes the first attempt;
- every declared artifact resolves to a safe relative path whose live SHA-256
  matches its recorded digest;
- attempts, case rows, exclusions and grader records account for the whole
  frozen schedule, with no silent drops;
- reported aggregates recompute from the retained rows;
- `status` matches the threshold comparison, and `pass` is claimed only when
  every applicable threshold and regression margin held;
- the enclosing delivery run, evaluation ID and plan digest match the binding
  the delivery receipt records.

`pass` is a machine assurance result for the named distribution, not general
safety or user acceptance. Attach it to the canonical delivery receipt; only
that lifecycle's user gate can mark the outcome accepted. Unsupported receipt
shapes fail closed; rerun them from a freshly frozen plan instead of migrating
or guessing missing evidence.
