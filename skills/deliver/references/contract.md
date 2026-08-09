# Delivery contract

`RUN.json` with `contract: delivery-run` and `schema_version: 1` is the single
canonical lifecycle receipt. The profile registry
declares minimum deterministic and judgement gates; project profiles may add
requirements. `status` is the live state and `state_history` is its ordered,
timestamped proof. Side states (`blocked`, `cancelled`, `degraded`) require a
reason and recovery instruction and cannot replace a mandatory gate.

## Producer and validator separation

`scripts/delivery_receipt.py` is the only writer of `RUN.json`;
`scripts/validate_delivery.py` is the only judge of it. Neither imports the
other. Both read `contract/lifecycle.py`, the shared written contract that owns
the state table, so the producer refuses an illegal transition at write time
and the validator rejects it independently at read time. That independence is
the guarantee: an agreement between two modules that share a checker is not the
same as one module checking itself.

Producer commands, all under `.agent-run/<id>/` and all taking an in-directory
lock: `init` (profile, tier, chair family, authority, risk assessment, intent),
`bind` (a section by digest), `artifact add` (hashes the live bytes),
`evidence run` (executes the command and binds exit code, bundle entry, bundle
digest and artifact digest in one transaction), `evidence human`,
`evidence remove` and `evidence rebuild` (the path back to consistency after an
aborted lane), `review add` (explicit lineage: adapter, provider family, model,
role, lenses, artifact and route-receipt digests), `checkpoint`, `transition`
and `show`.

Refusals happen at write time, not only at validation: timestamps come from the
process clock and strictly increase, evidence identifiers are minted against the
validator's pattern, no flag can supply an exit code, and a tier downgrade after
approval is refused. Do not hand-edit a receipt to work around one of these; the
refusal is the contract holding.

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
"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/validate_delivery.py" \
  .agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes \
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
`software_delivery` binding while it remains `awaiting_acceptance`. Its
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
