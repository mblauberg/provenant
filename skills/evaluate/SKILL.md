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
- candidate and applicable comparator manifests, sampling paired by task,
  seeds, repetitions, timeouts, retries and exclusions;
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
   Prefer execution oracles such as tests and type checks; use model graders
   only for rubric items those cannot capture. Run each pairwise model
   judgement in both orders; it counts only when both agree, else it is a tie.
6. Compare with frozen thresholds and applicable arms. A post-hoc change creates
   a fresh run; it does not rewrite the completed evidence. A single run, or a
   gap below about 5 percentage points on a pass-rate-style metric or below the
   plan's frozen minimum effect for any other metric, never justifies a routing
   or preset change.
7. Preserve bounded failures and route product defects to `implement`, unclear
   requirements to `scope`, and operational regressions to `diagnose`.

Never claim impossibility from a stochastic search: solve it exactly, or say
"best found in this sample".

Before finalising, apply the [receipt checklist](references/receipt.md#final-checklist):
verify frozen anchors, safe paths and live digests, complete accounting,
recomputable aggregates, thresholds and delivery binding.

`pass` is a machine assurance result for the named distribution, not general
safety or delivery acceptance. Attach it to the canonical delivery receipt;
the enclosing lifecycle combines objective checks with the review required
by the tier to meet acceptance. Explicit user acceptance is required only
for `terminal` work or when the approved scope asks for it. Unsupported receipt
shapes fail closed; rerun them from a freshly frozen plan instead of migrating
or guessing missing evidence.
