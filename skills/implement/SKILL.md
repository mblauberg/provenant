---
name: implement
description: "Use for approved software changes through verification, review, repair and acceptance. Use scope for unsettled scope, diagnose for investigation, code-review for read-only review and release for promotion."
---

# Implement

## Entry gate

Require approved scope, non-goals, acceptance criteria, approved or parked
costly-to-reverse decisions, build ceiling and write/external authority.
Require the HARNESS minimum tier and task-scoped paths, disclosure, secrets
and external-action constraints; assess evaluate for stochastic or judgement-bearing
behaviour. Missing inputs go to scope; infer no owner decisions beyond its
recorded, owner-authorised assumptions.

For active service or safety impact, user-approved containment may parallelise
verification and review. Keep every gate and name a reconciliation run.

## Loop

Read [delivery contract and execution detail](references/run-contract.md).

1. Substantial+ starts a fresh implementation session bound to approved digests.
   Save a delivery receipt with `provenant workflow receipt save`; carry its
   typed DeliveryId through every dispatch, review, retry, resume and handoff.
2. Keep an adaptive plan and name the governing skill in worker contracts.
   Writer tier follows task size. Use tdd for every new or changed observable
   behaviour, refactor for preserving structure,
   diagnose for unknown causes and orchestrate for decomposition.
   For version-sensitive interfaces and migrations, read
   [source grounding](references/source-grounding.md) and
   [migration compatibility](references/migration-compatibility.md).
3. Run fast checks before review; slow checks run in parallel with review.
   Map criteria to evidence and attach a passing evaluate receipt when
   required. Report project-deferred tests as unverified; their gate runs
   before merge.
4. Apply the contract's polish step for diffs over about 300 changed lines or
   multiple crates/packages, subject to `review.polish`. Keep changes within
   approved structure and every fast check green.
5. Invoke read-only code-review under the HARNESS ladder. Bind attempts with
   `provenant workflow review`. UI changes produce viewport screenshots;
   attach them to briefs for reviewers with vision. Record omissions and
   substitutions; native reviewers bind the tool's model parameter and
   record resolved routes from tool evidence.
6. Batch review and slow-check findings into bounded repair. A failed gate
   gets one repair at the same tier, then escalates one tier. Downward
   handoffs carry failing tests, types or checks. Re-review only repaired
   deltas. Contest findings under code-review's contract; one re-audit adds
   no gate. Stop and escalate after two consecutive repair cycles leave
   failures unchanged or larger. Every gate passes before merge.
7. Update owned docs for behavioural, architectural, operational or decision
   changes. Substantial+ uses session to refresh recovery checkpoints, audit
   context and graduate durable findings.
8. The PR is the evidence: exact-head CI, required review and repository merge.
   Hand off after the receipt accepts.

## Authority and completion

Reviewed source stays immutable. Separately authorised artefacts and Fabric
communication stay bounded. Missing review legs remain failed/unavailable;
unsatisfied HARNESS coverage blocks. Evidence outranks confidence; adjudicate,
never vote. Recheck absence findings against their cited evidence.

Objective checks and tier-required review meet acceptance; the chair reports
the outcome and the user may reject it. Explicit user acceptance is required
only for terminal work or when scoped; a project may name its landing gate.
Never fabricate approval from passing checks. Promotion needs separate release
authority. Return evidence to scope on drift. Keep criteria, evidence and
outcome in the existing project document or PR.

Reference eval cases (not run by CI): [lifecycle](evals/lifecycle_cases.yaml).
