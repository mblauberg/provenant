# Delivery contract

Delivery acceptance is met by objective checks and tier-required review; the
chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope
asks for it. A project may name its landing gate as acceptance. Record the
acceptance basis separately from human approval.

`RUN.json` with `contract: delivery-run` and `schema_version: 1` is the single
canonical lifecycle receipt. The profile registry
declares minimum deterministic and judgement gates; project profiles may add
requirements. The receipt is flat: it records what the run did, not the order
in which it did it. `delivery_run_shape.py` declares the field set and its
types, and both the producer and the validator check it, so an unknown,
missing or wrong-typed field is refused on write and on read. Progress is read
from the gates the receipt records: `intent.approval`, `design.status`,
`authority`, `delivery_acceptance`, `human_gates.acceptance`,
`human_gates.release` and `observation.status`. Each gate demands its own evidence; none of them
certifies an ordering the writer could not prove.

`writer_model` is an optional model identifier for the implementation writer.
Supply it to `delivery_receipt.py init --writer-model` only when the resolved
model is known; telemetry carries it into structured delivery stats.

## Acceptance basis

The optional schema-v1 `delivery_acceptance` object has exactly `basis` and
non-empty, unique `evidence_ids`. Bases are `checks-and-review`,
`project-landing` and `user`; every linked evidence row must pass. IDs cover
required profile gates and every passing review; `user` also binds the human
approval evidence. All bases activate the same profile, review, security, measures, evaluation and
high-stakes checks and freeze the same bound sections. Legacy receipts with
approved `human_gates.acceptance` remain valid without the new object.

Use `delivery_receipt.py accept --run-dir <dir> --basis checks-and-review
--evidence <check-id> --evidence <review-id>` after binding the required evidence.
The producer validates live hashes before recording acceptance. Project landing
uses `--basis project-landing` and requires either the verified merged-software
binding or deterministic `project-landing` evidence. The merge binder's
`--accept-as-project-landing` option records that basis after binding the exact
merged evidence when the project names landing as its acceptance gate.

`human_gates.acceptance` still means a human approved, with matching passing
human evidence. The human evidence producer records basis `user` for new human
acceptances. Initialise a scope-required user gate with
`init --requires-user-acceptance`, which records
`intent.requires_user_acceptance: true`. Terminal and scope-required acceptance
still demand human approval evidence. Acceptance never supplies release or
cleanup authority; release, observation and deletion guards remain separate.

## Repair budget

Record each repair with `delivery_receipt.py repair --reason ...
--failing CHECK_ID` for every gate that failed before the cycle. The shared
risk-tier ceiling is two cycles for `routine`, four for `substantial`, and five
for `crucial` or `terminal`. `validate_delivery.py` enforces
`repair_cycles`. The ceiling is a guardrail, not a target: stop and return
evidence to the user or `scope` when it is exceeded. A new requirement,
authority expansion or one-way-door decision returns to `scope`; it is not a
repair.

## Fabric relationship binding

New receipts declare the optional-in-v1 `fabric_relationships` object. Omission
is accepted only for backward compatibility with receipts created before this
extension. A coordinated delivery uses concrete bounded Fabric identifiers and
binds `delivery_run_id` back to the receipt's exact `run_id`:

```json
{
  "mode": "coordinated",
  "delivery_run_id": "DEL-001",
  "project_session_id": "ps_01",
  "coordination_run_id": "run_01",
  "workstream_id": "workstream_01",
  "lead_agent_id": "agent_lead_01"
}
```

An independent delivery uses the same complete shape with `mode` set to
`independent`, `delivery_run_id` still equal to `run_id`, and all four parent,
workstream and lead values set exactly to `not_applicable`. Partial objects,
unknown fields or modes, invented parents, invalid identifiers and a mismatched
delivery run fail closed. `lead_agent_id` identifies the bounded workstream
lead, never a second chair; live Fabric membership and chair authority remain
coordination state rather than being recreated in this receipt.

## Authority V2

The delivery receipt remains `delivery-run` schema version 1. Its nested
`authority` is a closed schema-version-2 object covering approval evidence,
workspace/source/artifact paths, denied paths, prohibited actions, disclosure,
secrets, deployment, irreversible actions, tool egress, expiry and budget.
Every field is required; disabled union arms use an explicit empty companion
list rather than an omitted value or default.

`authority.evidence_digest` must equal the digest of the artifact linked by the
passing human `authority-approval` evidence row. The pure
`scripts/authority_mapping.py` mapper validates and normalises that object for
containment checks; it performs no I/O or provider call.

Each `delegations[]` item contains `actor` plus a complete V2 scope. Its
normalised scope inherits the parent's exact approval binding and must narrow
every authority dimension. Partial delegations and any widening fail closed.

Non-Git artifact digests use `sha256:<64 lowercase hex>`. Local paths are
relative to the explicit workspace root; validate with `--workspace-root` and
`--verify-hashes`. External URIs require a non-empty
`digest_unavailable_reason` when bytes cannot be bound. New Git evidence uses
media type `application/x-git-revision`: its closed `git_revision` has exactly
`repository`, `commit` and `tree`, omits both digest fields, and cannot also
declare a path or URI. Verification detects the repository's native object
format, requires full-width lowercase object IDs, requires the declared object
itself to be a commit, and requires its available tree to equal the declared
tree. All evidence reads use the closed Git runner: inherited repository,
object and config routing is removed; replacements and grafts are disabled;
missing promisor objects are never fetched. Frozen schema-v1
`application/x-git-archive` receipts remain readable only with their SHA-256
archive digest; binders never emit that legacy form.

From the project root:

```sh
"$(provenant root)/skills/deliver/scripts/validate_delivery.py" \
  .agent-run/runs/<run-dir>/RUN.json --workspace-root "$PWD" --verify-hashes \
  --product-root "<product-root>"
```

Installed skill callers may instead export `AGENT_FABRIC_PRODUCT_ROOT`. The
explicit argument takes precedence and keeps product policy lookup independent
of the installed skills root.

User decisions link matching passing `kind: human` evidence. Deterministic
evidence records an exit code and a receipt digest equal to its declared
artifact digest; `--verify-hashes` then binds that artifact to its live bytes.
Non-evaluation deterministic evidence must be a local JSON
`deterministic-evidence-bundle`; its exact check IDs, gates, statuses, methods,
source paths and exit codes must match the linked evidence rows.
Judgement evidence records actual model lineage. Non-user evidence lists the
source paths it consumed, all within authority. A digest-bound
`--project-policy` may add a complete project profile or add requirements to a
built-in profile, never remove them.

Stochastic assurance uses a lifecycle binding with exactly `status`,
`anchored_at`, `evidence_id`, `evaluation_artifact_id`, `evaluation_id`,
`evaluation_digest` and `plan_digest`. Before execution, a `planned` row binds
the evaluation ID and frozen plan digest; artifact, evaluation digest and
evidence fields stay empty. `complete`, `failed` and `incomplete` rows keep
that anchor and fill the three live-result fields. Complete rows link passing
judgement evidence. Terminal nonpasses link deterministic receipt-validation
evidence and remain in the history; they never satisfy stochastic assurance.
Awaiting acceptance requires at least one complete passing row and no planned
row.

The referenced local JSON artifact must be declared as evidence. With
`--verify-hashes`, the delivery validator checks its live digest and invokes
the canonical `evaluation-run` validator with the anchored evaluation ID,
frozen plan digest and enclosing delivery run ID. It also verifies every
artifact inside that evaluation receipt and checks profile minima against the
bound plan for complete candidates. Terminal nonpasses may fall below those
minima because they are retained evidence, not acceptance candidates. For
every materialised row, the plan must be frozen no later than `anchored_at`,
and the anchor must precede that nested evaluation's earliest preflight or
attempt. A retry may therefore be freshly anchored after an earlier evaluation
fails, even while the enclosing delivery remains in execution. Copied dataset,
sample, repetition, threshold or score fields cannot satisfy this gate.

Security checks are selected by changed surface. Crucial software and
agent-product runs cannot reach acceptance without matching passing
deterministic results. Agent products disposition the OWASP agentic risk
catalogue; `not_applicable` needs a reason.

Observation is profile-specific but always names window, signals, thresholds,
owner, containment, privacy and close condition. `not_applicable` requires a
profile justification.

## Merged software binding

After a pull request merges, a software receipt may add the closed
`software_delivery` binding, but only before delivery acceptance is recorded. Its
canonical artifact uses new-form `git_revision` (`repository`, `commit`,
`tree`) with no second archive or per-file digest. Validation applies the exact
native-object and closed-runner rules above and rejects digest fields on this
new media type. The binding also
names local, digest-verified JSON artifacts with contracts
`github-pull-request-evidence`, `github-ci-evidence` and
`code-review-evidence`. The PR binds reviewed head to merge commit; required
`ci-status` binds the merge commit; every passing primary review is retained;
and the merged tree must equal the reviewed head tree.

This is an additive schema-v1 extension: older delivery receipts still
validate for historical readability, but release rejects software promotion
without the post-merge binding. A URI or a reconstructed post-acceptance draft
cannot substitute for these local typed artifacts.

The profile gate uses independently authored positive, negative and boundary
cases rather than receipts emitted by the reference generator:

```sh
provenant check
```

The harness gate includes the delivery scenario replay. Every expected outcome
must match. The dataset covers each base profile in both directions, exercises
the high-stakes overlay twice, and repeats stochastic or boundary cases where
reproducibility matters.
