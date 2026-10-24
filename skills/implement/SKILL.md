---
name: implement
description: "Use for an approved software change through verified implementation, independent review, bounded repair, and acceptance. Not for unsettled scope, diagnosis-only, read-only review, or release."
---

# Implement

## Entry gate

Require:

- approved scope, non-goals and acceptance criteria;
- costly-to-reverse decisions approved or expressly parked;
- build ceiling and write/external authority;
- `config/risk-policy.json` risk/authority profile: minimum tier, bounded paths,
  disclosure, secrets and external-action constraints;
- `evaluate` need for stochastic/judgement-bearing behaviour.

If missing, use `scope`; never infer owner decisions beyond the recorded,
owner-authorised assumptions `scope` allows.

For active service or safety impact, user-approved containment may parallelise
verification and review. Gates and authority apply; name a
reconciliation run.

## Loop

1. For substantial+ work, record a delivery receipt with `provenant workflow
   receipt save` and follow [run-contract.md](references/run-contract.md).
   Carry the typed `DeliveryId` in every lane dispatch, reviewers included;
   retries, resumes and handoffs inherit it.
2. Keep an adaptive plan. Name the governing skill in the worker contract
   before changing code. Invoke `tdd` for every new or changed observable
   behaviour, and use `refactor`
   for behaviour-preserving structure and `diagnose` for unknown causes.
   Writer tier follows task size; a failed gate gets one repair at the same
   tier, then escalates one tier. A downward handoff carries an executable
   contract: a failing test, types or a check command.
   Migrations may need behaviour tests and equivalence evidence. Use
   `orchestrate` when decomposition helps; adapt topology inside authority.
   For a version-sensitive external interface or migration, apply
   [source grounding](references/source-grounding.md) and
   [migration compatibility](references/migration-compatibility.md).
3. Run fast checks (lint, types, targeted tests) and pass them before review.
   Slow checks (full suites, CI) run in parallel with review. Map each
   criterion to evidence. When project instructions defer test execution to
   a later gate, report those tests as deferred and unverified; that gate
   runs them before merge. When required, run `evaluate` and attach its
   passing receipt.
4. Invoke read-only `code-review` under the current `HARNESS.md` risk ladder.
   Bind each reviewer attempt with `provenant workflow review`. A change that affects UI produces screenshots at set viewports and
   attaches them to the review brief; reviewers with vision get them. Record
   omissions and substitutions. Native subagents use the Agent tool model
   parameter and mark the route resolved.
5. Batch findings from review and the slow checks into one repair round, then
   repeat verification and re-review only the repair delta. A gate that still
   fails takes the step 2 escalation, within the repair ceilings in
   [run-contract.md](references/run-contract.md). Stop and escalate, or return to the user, when two
   consecutive repair cycles leave the failing-check set unchanged or larger,
   even with budget left.
   Every gate still passes before merge. Return
   evidence to user or `scope` on drift. `review.rereview_scope` and the other
   `review.*` knobs in `scripts/provenant-profile show --resolved` adjust this step.
6. Update owned docs for behavioural, architectural, operational or decision
   change. For substantial+ apply `session`: refresh recovery checkpoint,
   audit context and graduate durable findings.
7. The pull request is the evidence: exact-head CI, required review and the
   repository merge. Hand off after the receipt accepts.
8. Objective checks and the review required by the tier meet final acceptance;
   the chair reports the outcome and the user may still reject it. Require
   explicit user acceptance only for `terminal` work or when the approved
   scope asks for it. A project may name its landing gate as acceptance.
   Promotion needs separate `release` authority.

## Authority and completion

- Reviewed source stays immutable; separately authorised artifacts and Fabric
  communication stay bounded.
- Record missing legs as failed/unavailable; never filter them. Review coverage
  blocks whenever the HARNESS ladder is unsatisfied.
- Objective evidence outranks reviewer confidence; adjudicate conclusions,
  never vote. Recheck an absence finding's cited evidence before it blocks.
- Substantial+ starts a fresh implementation session bound to approved digests.
- Do not infer a user approval from passing checks; record it only when the
  user gives it.

## Project record

Keep the scoped criteria, evidence and outcome in the existing project document
or PR.
