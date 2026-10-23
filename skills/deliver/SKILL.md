---
name: deliver
description: "Use for taking an approved research, analysis, document, or agent-product outcome through evidence, review, and acceptance. For a software change, unsettled scope, or release, use implement, scope, or release."
---

# Deliver

Coordinate the domain-neutral lifecycle; domain skills own their methods.

## Entry

Require an approved intent, acceptance criteria, minimum risk tier and bounded
authority. Scope/design, disclosure, one-way doors and risk downgrades need
explicit user approval. If intent is unsettled, use `scope`.

State scoped criteria in the PR or skill document. Evaluate owns repetition
protocols; release and domain skills own their evidence and approval duties.
Use risk-tier repair ceilings as guidance, without a delivery profile registry.

## Lifecycle

1. From the authorised workspace root, create `.agent-run/runs/<YYYYMMDD-HHMM>-delivery-<slug>-<rand6>/RUN.json`.
   For a Git linked worktree, resolve the primary checkout with
   `dirname "$(git rev-parse --path-format=absolute --git-common-dir)"`
   and run the producer there. Keep intent, authority and evidence paths under
   that primary checkout; source edits may remain in the linked worktree:
   ```sh
   "$(provenant root)/skills/deliver/scripts/delivery_receipt.py" init \
     --run-dir ".agent-run/runs/<YYYYMMDD-HHMM>-delivery-<slug>-<rand6>" --run-id "<id>" \
     --chair-family "<family>" --risk-assessment "<risk-assessment.json>" \
     --intent "<approved-intent-file>" --authority "<authority.json>"
   ```
   The intent file must be non-empty; `--risk-assessment` and `--authority`
   accept a JSON file path or inline JSON. Add `--fabric-relationships` only
   for coordinated work, then bind other required artifacts through the same
   producer and follow
   [the receipt contract](references/contract.md).
   Pass that `RUN.json` run_id as `delivery_run_id` on every new lane
   dispatch for the delivery, reviewers included; retries, resumes and handoffs
   inherit the id.
2. Record each state transition. No state may jump an approval, evidence,
   review, acceptance or release gate.
3. Execute through relevant skills. Software routes through `implement`;
   stochastic behaviour through `evaluate`; failures use `diagnose`;
   substantial parallel work uses `orchestrate`.
4. Produce the scoped deterministic evidence before judgement evidence.
   Every gate links to a typed artifact or receipt. At acceptance, a stochastic
   gate must bind and hash-verify a passing canonical `evaluation-run` receipt;
   copied scores or sampling metadata are not evidence. Retain failed or
   incomplete evaluation receipts as non-gating history.
5. Review independently with lenses from the dependency cone under the
   `HARNESS.md` risk ladder. Record `adapter/model@effort` from each Fabric
   attempt receipt; derive family. Native Claude subagents record
   `claude/<model>@<effort> (anthropic; resolved)` from the Agent tool model
   parameter. Record skipped legs.
6. Repair under the risk-tier scaled budget in [the receipt
   contract](references/contract.md). Scope/design drift returns to the user
   gate.
7. Validate from the project root with
   `"$(provenant root)/skills/deliver/scripts/validate_delivery.py"
   .agent-run/runs/<YYYYMMDD-HHMM>-delivery-<slug>-<rand6>/RUN.json --workspace-root "$PWD" --verify-hashes` (plus
   digest-bound `--project-policy` when used).
   Acceptance is met by objective checks and the review required by the tier.
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

`implement` records the same minimal subject, authors, risk, review references
and decision; no parallel implementation receipt format exists. Live task or
membership projections may
link to it but never replace its canonical acceptance evidence.

For audience-ready HTML, apply the
[interactive-document boundary](references/interactive-documents.md).

## Project record

Keep the scoped criteria, evidence and outcome in the existing project document
or PR. A context stamp does not establish acceptance or authority.
