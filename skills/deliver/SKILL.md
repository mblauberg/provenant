---
name: deliver
description: "Use to verify and accept approved research, analysis, documents or agent work. Use implement for software changes, scope for unsettled work, and release for promotion."
---

# Deliver

Coordinate the domain-neutral lifecycle; domain skills own their methods.

## Entry

Require an approved intent, acceptance criteria, minimum risk tier and bounded
authority. Scope/design, disclosure, one-way doors and risk downgrades need
explicit user approval. If intent is unsettled, use `scope`.

Classify the work as `software`, `research`, `analysis`, `document` or
`agent-product`; add a high-stakes overlay when source authority, privacy or
qualified review matters. Projects may strengthen the gates, never weaken them
silently.

## Lifecycle

1. Mint a typed `DeliveryId` and carry it in every lane dispatch for the
   delivery, reviewers included; retries, resumes and handoffs inherit it.
   Record the receipt with `provenant workflow receipt save <id>
   --expected-revision N --request <file>`; read it with `provenant workflow
   receipt show <id>`. See [the receipt contract](references/contract.md).
2. Keep the receipt truthful at each gate. No state may jump an approval,
   evidence, review, acceptance or release gate.
3. Execute through relevant skills. Software routes through `implement`;
   stochastic behaviour through `evaluate`; failures use `diagnose`;
   substantial parallel work uses `orchestrate`.
4. Produce deterministic evidence before judgement evidence. Every gate links
   to a typed artifact or receipt. A stochastic gate binds a passing
   `evaluate` receipt; copied scores are not evidence.
5. Review independently with lenses from the dependency cone under the
   `HARNESS.md` risk ladder. Bind each reviewer attempt with `provenant
   workflow review --request <file>`, which reads the attempt from run custody
   and checks coverage. Record skipped legs.
6. Repair under the risk-tier scaled budget in [the receipt
   contract](references/contract.md). Scope/design drift returns to the user
   gate.
7. Accepting a substantial delivery is refused (`REVIEW_INCOMPLETE`) until
   `workflow review` coverage passes. Acceptance is met by objective checks
   and the review the tier requires.
8. The chair reports the outcome; the user may still reject it. Explicit user
   acceptance is required only for `terminal` work or when the approved scope
   asks for it. A project may name its landing gate as acceptance. External
   release needs separate user authority. Define observation
   before release; close only after its evidence window passes. Feed incidents
   into `retrospect`.

## Boundaries

Delegates may only narrow authority. One writer owns each shared source
surface. Artifact manifests classify `canonical`, `evidence`, `handoff`,
`scratch` or `external`; cleanup removes only expired, run-owned scratch with
explicit authority. Filesystem receipts remain truth when Herdr or another
transport is unavailable.

`implement` uses the same receipt; no parallel format exists. Live task or membership projections may
link to it but never replace its canonical acceptance evidence.

For audience-ready HTML, apply the
[interactive-document boundary](references/interactive-documents.md).

## Project record

Keep the scoped criteria, evidence and outcome in the existing project document
or PR.

Reference eval cases (not run by CI): [lifecycle cases](evals/lifecycle_cases.yaml).
