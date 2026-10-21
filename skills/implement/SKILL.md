---
name: implement
description: "Use for an approved software change through verified implementation, independent review, bounded repair, and user acceptance. Not for unsettled scope, diagnosis-only, read-only review, or release."
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

1. For substantial+ work, create the canonical `delivery-run` with profile
   `software` through the complete installed-producer command in `deliver`,
   then follow [run-contract.md](references/run-contract.md). Routine minor work may proceed
   without `RUN.json` unless the user or project policy requests one.
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
   a later gate, report those tests as deferred and unverified; that gate runs them before merge. When required,
   run `evaluate` and attach its passing receipt.
4. Invoke read-only `code-review` under the current `HARNESS.md` risk ladder.
   Record each `adapter/model@effort` route from its attempt receipt and derive
   family. A change that affects UI produces screenshots at set viewports and
   attaches them to the review brief; reviewers with vision get them. Record
   omissions and substitutions. Native subagents use the Agent tool model
   parameter and mark the route resolved.
5. Feed findings from review and the slow checks into one repair round, then
   repeat verification and re-review only the repair delta, under the `deliver`
   receipt contract's scaled budget. Every gate still passes before merge.
   Return evidence to user or `scope` on drift.
6. Update owned docs for behavioural, architectural, operational or decision
   change. For substantial+ apply `session`: refresh recovery checkpoint,
   audit context, graduate durable findings and classify retained/ephemeral
   artifacts in `RUN.json`.
7. When a receipt exists, validate with
   `"$(provenant root)/skills/deliver/scripts/validate_delivery.py" \
   .agent-run/runs/<YYYYMMDD-HHMM>-delivery-<slug>-<rand6>/RUN.json --workspace-root "$PWD" --verify-hashes`.
   Hand off only after this machine gate.
8. User final acceptance is mandatory; promotion needs separate `release`
   authority. Terminalise any run directory as
   [run-contract.md](references/run-contract.md) sets out.

## Authority and completion

- Reviewed source stays immutable; separately authorised artifacts and Fabric
  communication stay bounded.
- Record missing legs as failed/unavailable; never filter them. Review coverage
  blocks whenever the HARNESS ladder is unsatisfied.
- Objective evidence outranks reviewer confidence; adjudicate conclusions,
  never vote.
- Substantial+ starts a fresh implementation session bound to approved digests.
- Clearing every machine gate is not acceptance. Record the receipt's
  `human_gates.acceptance` approval only after explicit user acceptance.

## Portable artifact

Use the skill-owned [implementation checkpoint schema](portable-workflow.v1.json).
