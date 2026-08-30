---
name: deliver
description: "Use for taking an approved research, analysis, document, or agent-product outcome through evidence, review, and user acceptance. Not for software-only work, unsettled scope, or release; use implement, scope, or release."
---

# Deliver

Coordinate the domain-neutral lifecycle; domain skills own their methods.

## Entry

Require an approved intent, acceptance criteria, minimum risk tier and bounded
authority. Scope/design, disclosure, one-way doors and risk downgrades need
explicit user approval. If intent is unsettled, use `scope`.

Select base profile from `config/delivery-profiles.json`: `software`,
`research`, `analysis`, `document` or `agent-product`. Add high-stakes
overlay when source authority, privacy or qualified review matters. Projects
may strengthen a profile, never weaken kernel gates silently. Use explicit
independent `not_applicable` form for fabric_relationships rather than
inventing.

## Lifecycle

1. From the authorised workspace root, create `.agent-run/<id>/RUN.json`:
   ```sh
   "${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/delivery_receipt.py" init \
     --run-dir ".agent-run/<id>" --run-id "<id>" --profile "<profile>" \
     --chair-family "<family>" --risk-assessment "<risk-assessment.json>" \
     --intent "<approved-intent-file>" --authority "<authority.json>"
   ```
   The intent file must be non-empty; the other inputs are JSON files or JSON
   arguments. Add `--fabric-relationships` only for coordinated work, then bind
   other required artifacts through the same producer and follow
   [the receipt contract](references/contract.md).
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
