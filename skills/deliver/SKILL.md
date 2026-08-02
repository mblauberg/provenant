---
name: deliver
description: "Use for taking an approved research, analysis, document, or agent-product outcome through evidence, review, and user acceptance. Not for software-only work, unsettled scope, or release; use implement, scope, or release."
---

# Deliver

Run the domain-neutral lifecycle kernel. This skill coordinates existing
capabilities; domain skills own domain methods.

## Entry

Require an approved intent, acceptance criteria, minimum risk tier and bounded
authority. Scope/design, disclosure, one-way doors and risk downgrades need
explicit user approval. If intent is unsettled, use `scope`.

Select base profile from `config/delivery-profiles.json`: `software`,
`research`, `analysis`, `document` or `agent-product`. Add high-stakes
overlay when source authority, privacy or qualified review matters. Projects
may strengthen a profile, never weaken kernel gates silently. Use independent
`not_applicable` form for fabric_relationships rather than
inventing.

## Lifecycle

1. Create `.agent-run/<id>/RUN.json` with
   `scripts/delivery_receipt.py init`; the template is a read-only contract
   fixture. Bind intent, design and authority by digest. When coordinated, bind
   `fabric_relationships` to project session, coordination run and workstream
   IDs per [the receipt contract](references/contract.md).
2. Record each state transition. No state may jump an approval, evidence,
   review, acceptance or release gate.
3. Execute through relevant skills. Software routes through `implement`;
   stochastic behaviour through `evaluate`; failures use `diagnose`;
   substantial parallel work uses `orchestrate`.
4. Produce profile-required deterministic evidence before judgement evidence.
   Every gate links to a typed artifact or receipt. At acceptance, a stochastic
   gate must bind and hash-verify a passing canonical `evaluation-run` receipt;
   copied scores or sampling metadata are not evidence. Retain failed or
   incomplete evaluation receipts as non-gating history.
5. Review independently with lenses from the dependency cone.
   Substantial+ follows `HARNESS.md`: targeted lenses plus the other primary;
   distinct-family review when available, with terminal pressure made
   stronger and skipped optional legs recorded.
6. Repair under a risk-tier scaled budget defined by the `implement` skill's
   run contract. Scope/design drift returns to the user gate.
7. Validate from the project root with
   `"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/validate_delivery.py"
   .agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes` (plus
   digest-bound `--project-policy` when used).
   `awaiting_acceptance` is machine-ready, not complete.
8. User acceptance and external release are separate. Define observation
   before release; close only after its evidence window passes. Feed incidents
   into `retrospect`.

## Boundaries

Delegates may only narrow authority. One writer owns each shared source
surface. Artifact manifests classify `canonical`, `evidence`, `handoff`,
`scratch` or `external`; cleanup removes only expired, run-owned scratch with
explicit authority. Filesystem receipts remain truth when Herdr or another
transport is unavailable.

`implement` uses this same receipt with profile `software`; no parallel
implementation receipt format exists. Live task or membership projections may
link to it but never replace its canonical acceptance evidence.

For audience-ready HTML, apply the
[interactive-document boundary](references/interactive-documents.md).

## Adapter-absent path

Console, Herdr and GitHub are optional. Continue from canonical project
artifacts and emit the skill-owned artifact kind in
[portable-workflow.v1.json](portable-workflow.v1.json). That filesystem
artifact proves only that a context object existed. Retain and identify the
canonical context separately; this output is not itself workflow evidence,
acceptance or release authority.
