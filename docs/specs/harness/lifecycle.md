# Adaptive agent harness lifecycle

[Issue #23](https://github.com/mblauberg/provenant/issues/23) records the
completed delivery that established these requirements. A change to them
requires a current linked GitHub issue and Project Status.

The current contract permits direct read-only analysis of local session history, keeps sharing and export separately gated, and treats unsupported or unattributable evidence as `N/A`, never zero. It does not retain the synthetic collector, which had no provider-native adapter or producer. Route evaluation remains task-local, receipt-bound and content-free; it does not approve a learned or Pareto router.

## Authority and decision

The user instruction on 10 July 2026 approved this specification, authorised
the complete harness refactor and authorised a repository commit. It permits
reversible repository changes, tests and documentation within the existing
harness. It does not authorise provider login, external communications,
deployment, live installation, destructive migration, Git push or release.

This specification is grounded in
[`docs/research/agentic-sdlc-harness-2026.md`](../../research/agentic-sdlc-harness-2026.md).
Research claims expire for decision purposes after 90 days or when a cited
standard/provider interface materially changes, whichever occurs first.

## Problem

The repository has a mature agentic software loop but its top-level claim is
broader. Non-code delivery has no shared executable contract, design approval
can be unbound from a design artifact, continuous improvement is prose-only,
and the skill-evidence policy treats requested local inspection like exported
research telemetry. Security, observation, installation reconciliation and
trigger evaluation also need stronger machine evidence.

A large all-in-one workflow would increase context cost and couple every domain
to Git. The target is a small stable kernel plus profiles and existing skills.

## Objectives

- Make the lifecycle usable for software, research, analysis, documents and
  high-stakes domain work.
- Keep `implement` as the software entrypoint while removing Git assumptions
  from the shared contract.
- Bind scope, authority, design, evidence, review, acceptance, release,
  observation and improvement through typed receipts.
- Keep users at consequential intent, one-way-door, disclosure, acceptance and
  promotion gates.
- Make harness improvement measurable, privacy-safe and regression-tested.
- Keep Claude Code and Codex equal primaries; use other families as additive
  dissent, never uncorroborated blockers.
- Keep entrypoints compact and provider-neutral.

## Non-goals

- Replacing domain skills with one generic prompt.
- Mandating multiple agents for sequential or low-risk work.
- Storing full provider transcripts as project truth.
- Autonomous modification of the global harness.
- Making optional providers, Herdr, Pi or the agent fabric prerequisites for a
  normal run.
- Replacing project-specific legal, compliance or release authority.
- Reimplementing the separately specified shared agent fabric.

## Lifecycle model

```text
context
  -> intent and risk
  -> [USER: material scope/design]
  -> authorised delivery profile
  -> deterministic evidence
  -> behavioural/domain evaluation where needed
  -> independent multi-lens review
  -> bounded repair loop
  -> [USER: final acceptance]
  -> [USER: external release/promotion]
  -> observation window
  -> retrospective proposal and regression
  -> next intent
```

A failed deterministic or review gate returns to delivery. A finding that
changes accepted intent, authority or a one-way-door design returns to the
user design gate. Observation can open `diagnose`, incident response or a new
delivery cycle. No status may jump a missing gate.

## Target architecture

### Delivery kernel

The delivery kernel is a domain-neutral contract, validator and stable state
machine. It orchestrates existing capabilities; it does not contain domain
expertise:

```text
draft -> scoped -> approved -> executing -> verifying -> reviewing
      -> repairing -> awaiting_acceptance -> accepted
      -> awaiting_release -> observing -> closed
```

`blocked`, `cancelled` and `degraded` are side states with reasons and recovery
instructions. `degraded` never disguises a mandatory missing gate.

`implement` remains the software profile and supported direct entry point. It
uses the same canonical receipt; there is no parallel implementation schema or
compatibility adapter. The
pre-change held-out baseline scored 30/45 because every cross-domain delivery
case lacked a lifecycle entrypoint; the `deliver` catalogue scored 45/45
without displacing `implement`, `scope` or `release`. The public `deliver`
entrypoint is therefore selected. Evidence:
[`lifecycle-routing-baseline-2026-07-10.md`](../../research/lifecycle-routing-baseline-2026-07-10.md).

### Profiles

| Profile | Primary artifacts | Deterministic evidence | Judgement evidence | Release meaning |
|---|---|---|---|---|
| `software` | source, migration, config, docs | tests, build, types, lint, security scans, revision/diff | code review, UX/architecture rubric | merge/deploy/publish |
| `research` | report, dataset, evidence map | source existence, citation/claim coverage, reproducible transforms | source quality, synthesis, uncertainty, dissent | share/publish/use decision |
| `analysis` | report, model, table, visualisation | input manifest, calculation/recalculation, assumptions and sensitivity | interpretation, uncertainty and decision fit | share/use decision |
| `document` | Markdown, DOCX, PDF, slides, sheet | schema, formulas, links, render/page checks | accuracy, readability, visual and audience fit | send/file/publish |
| `agent-product` | prompts, tools, policies, eval sets, deployment config | unit/integration/security tests, version and permission checks | independent product review; behavioural evaluation/red team when the changed behaviour requires it | staged activation and monitored operation |

Projects may add a complete profile or strengthen a built-in profile through a
digest-bound additive policy. The global registry loads first; a project policy
can add evidence or measure requirements but cannot remove or reclassify global
minima. New profiles compose globally classified artifact types; a new artifact
type requires an explicit global surface-metadata decision. Profiles declare
artifact types, deterministic and judgement gates,
outcome/trajectory measures, stochastic minima, permitted security surfaces,
boundary checks, evidence retention/redaction and release semantics.

An `agent-product` label does not itself make every change stochastic. Tests,
permission checks and applicable security boundaries remain mandatory;
prompt, tool, policy and evaluation-set artifacts require behavioural
evaluation, while a deployment-config-only artifact may use the deterministic
path. The validator derives that decision from canonical artifact types rather
than trusting a free-form receipt claim. When selected, the profile's
repeated-trial and sample-size minima still apply.

High-stakes work is an orthogonal safeguard, not a file type: it adds source
authority, privacy, qualified-domain review and explicit user-action gates to
any base profile.

### Skills remain composable

`scope`, `prototype`, `tdd`, `diagnose`, `evaluate`, `code-review`, `release`,
`session`, `work-map`, `orchestrate` and domain skills remain independently
triggerable. The kernel (or a future `deliver` entrypoint) calls only what the
risk and profile require. A tiny answer does not create a run directory merely
to satisfy ceremony.

## Neutral run receipt

The canonical receipt remains `.agent-run/<run-id>/RUN.json`, using the single
public `delivery-run` schema v1.
The single location avoids parallel lifecycle truth beside orchestration and
agent-fabric receipts. JSON is used for
validation; human-readable artifacts remain Markdown or native documents.

The following excerpt omits unchanged fields from the full template:

```yaml
schema_version: 1
contract: delivery-run
run_id: DEL-001
profile: research
status: reviewing
risk_tier: substantial
intent:
  artifact: docs/specs/example.md
  digest: sha256:...
  decision_owner: human-maintainer
  approval:
    status: approved
    approver: human-maintainer
    evidence: intent-approval
authority:
  approved_by: human-maintainer
  evidence: authority-approval
  allowed_source_paths: [docs/research/]
  allowed_artifact_paths: [docs/, .agent-run/DEL-001/]
  prohibited_actions: [external-publish, commit]
  disclosure: local-only
artifacts:
  - id: report
    path: docs/research/example.md
    media_type: text/markdown
    artifact_type: report
    digest: sha256:...
evidence:
  - id: citation-coverage
    kind: deterministic
    gate: source-coverage
    method: scripts/check-claims
    status: pass
    artifact_id: evidence-bundle
    source_paths: [docs/research/]
    result: {exit_code: 0, receipt_digest: 'sha256:...'}
reviews:
  - provider_family: anthropic
    independent_of_authorship: true
    lenses: [source-quality, synthesis, uncertainty]
    status: pass
human_gates:
  acceptance: {status: pending}
observation: {status: planned, window: {kind: event-count, minimum: 1}}
```

Required invariants:

- an approved intent has a stable artifact or embedded statement, digest,
  decision owner, approver and evidence reference;
- every artifact has a path/URI, media type and digest or an explicit reason a
  digest is impossible;
- every gate links to evidence, not only a status;
- authority may be narrowed by delegates but not broadened;
- actual model/provider lineage is recorded when model work affects a gate;
- reviewer independence is explicit;
- distinct-family failure records a reason but cannot replace the other-primary gate;
- acceptance and release are separate;
- a profile validator may add requirements but not remove kernel invariants.

## Design and risk gate

Risk tiers remain `routine`, `substantial`, `crucial` and `terminal`. Review
pressure follows [`HARNESS.md`](../../../HARNESS.md); this specification does
not duplicate the tier ladder.
Substantial, crucial and terminal runs require an intent/design artifact.
Crucial and terminal runs additionally require alternatives, threat/failure
analysis, rollback or containment, unresolved decisions and named user
approval; terminal keeps the strongest review and external-action gates in
`config/risk-policy.json`.

The validator rejects:

- `approved` with a missing artifact, digest, approver or approval evidence;
- an artifact modified after its recorded approval digest;
- an unresolved one-way-door decision marked as implementation detail; and
- a risk downgrade without user evidence.

## Verification and review

### Verification plan

Each profile declares:

- deterministic gates and commands/methods;
- stochastic evaluators, datasets, repetitions and thresholds;
- outcome and trajectory measures;
- security checks selected from changed surfaces;
- artifact rendering or source-boundary checks; and
- evidence retention and redaction.

Deterministic checks run first. Stochastic checks record model, prompt/rubric,
dataset version, sample size, aggregation and raw-evidence location. A single
model verdict cannot be labelled reproducible.

### Multi-lens review

Review selects non-overlapping lenses from correctness, specification
alignment, security, privacy, performance, reliability/concurrency, state/type
boundaries, test/eval coverage, accessibility, evidence quality,
readability/maintainability and structural simplification.

Substantial work requires multiple targeted lenses and the other primary family.
Crucial work uses a distinct family when available. Terminal work increases
targeted and adversarial pressure and records any skipped distinct-family leg.
Reviewers work independently before synthesis. The reducer adjudicates against
evidence and records disagreement; no majority vote can override a deterministic
failure or user authority.

## Local skill evidence and shared exports

`skill-craft`'s audit branch defaults to static analysis. A direct user request authorises
read-only, in-place analysis of the named local session histories. When the
provider roots and useful window are unambiguous from that request and the live
environment, the agent proceeds without a second receipt, redaction pass,
retention date or minimum-cell gate. Raw histories remain local, are never
committed and do not become project truth.

A compact aggregate or paraphrased report to the requesting user in the same
authorised session is local delivery, not sharing/export, and requires no
second disclosure confirmation. Run-owned local scratch is also allowed.
Creating a persistent repository/shared artifact, sending raw excerpts to
another provider, or disclosing to a new audience or external destination
requires separate authority. Once authorised, the user confirms the audience,
destination and whether excerpts are allowed; output excludes secrets and
out-of-scope third-party private content.

Invocation, correction and completion claims require structured attribution or
user-reviewed, provenance-valid evidence. Loading a skill is not selection.
Unsupported or unattributable evidence is `N/A`, never zero. The harness claims
no generic native-provider history collector until real adapters and producers
exist. History predating a skill may inform broad harness patterns but cannot
score that skill; those cells remain `N/A`. The balanced local-history fixture
is prospective contract coverage, not a measured production selection rate.

## Measurable retrospective

Every substantial completed cycle may produce `RETROSPECT.json`. Crucial,
escaped-defect and repeated-correction cycles require it. Human corrections are
timestamped events linked to matching human evidence; technical repair counts
remain a separate signal.

Required fields are cycle/profile, evidence window, baseline or explicit
absence reason, comparable run or absence reason, outcome and trajectory
measures, root-cause clusters with evidence IDs, proposed changes, authority,
regression gates, canonical destinations and next-cycle recurrence checks.

A proposal is not `verified` until its regression gate passes. An improvement
is not `effective` until a comparable later cycle measures recurrence and
checks for regressions/cost transfer. The validator forbids raw transcript
payloads and dated diary destinations. `no-change` is valid when evidence
supports it.

## Security evidence

Software and agent-product profiles select deterministic checks based on the
changed surface:

- secrets and sensitive-data scanning;
- dependency/advisory and licence checks;
- language SAST and unsafe-code rules;
- IaC/container/config policy checks;
- generated artifact and provenance checks; and
- tests for authentication, authorisation and destructive boundaries.

At substantial risk and above, any profile containing a software or
agent-product artifact type maps every canonical artifact to its type-derived
minimum surfaces; custom profile names cannot suppress required checks.

Agent-product work also maps applicable OWASP agentic risks: goal hijack, tool
misuse, excessive privilege, supply chain, code execution, memory/context
poisoning, insecure inter-agent communication, cascading failures and human
trust exploitation. `not_applicable` requires a reason. Tool/model review
cannot substitute for missing deterministic evidence.

## Observation and incidents

Release defines an observation contract before promotion:

```yaml
window: {kind: duration, minimum_seconds: 86400}
signals: [availability, error-rate, task-success, policy-violations]
thresholds:
  availability: {direction: gte, limit: 99.9}
  error-rate: {direction: lte, limit: 1}
  task-success: {direction: gte, limit: 95}
  policy-violations: {direction: eq, limit: 0}
owner: human-maintainer
rollback_or_containment: docs/runbooks/example.md
sampling_and_privacy: aggregate-redacted
close_condition: all thresholds pass for the window
evidence_ids: [observation-report]
```

Non-production profiles use an appropriate analogue, such as a citation audit,
recipient confirmation, registry acceptance or decision follow-up. Observation
may be `not_applicable` only with profile justification. Incidents link the
release, evidence window, containment, diagnosis and resulting regression case.

## Installation, precedence and portability

Introduce a versioned installation manifest containing skill name, source
digest, installed target, ownership and rename/supersession history. Installer
operations support `plan`, `install`, `reconcile` and `uninstall-managed`.
Different unmanaged existing paths are never claimed or overwritten. An
unmanaged file with bytes identical to the product source may be adopted as a
managed link. Broken managed links and safe managed renames are repaired with
receipts. The target-bound
manifest hashes full skill-tree bytes and executable modes; link mutations roll
back if its atomic commit fails. Conflicts stop for user resolution.

Instruction precedence is one sentence across all entrypoints:

> Platform/system policy and explicit user authority lead; the nearest
> project instruction may specialise or strengthen the global harness but may
> not silently broaden authority, weaken safety gates or redefine global
> cross-project memory policy.

Provider-specific adapters advertise capabilities. Skills depend on capability
contracts, not vendor names. Herdr and the shared agent fabric are optional
transports; filesystem artifacts and receipts remain portable truth.

## Context and artifact lifecycle

Each run owns an artifact manifest with class, owner, retention and expiry:

- `canonical`: curated project truth; never automatically deleted;
- `evidence`: retained by profile/risk policy, redacted where required;
- `handoff`: compacted or graduated when the effort closes;
- `scratch`: run-owned and safe to remove after the recorded expiry; and
- `external`: referenced, not copied unless licence and disclosure permit it.

Session/context audit reports oversized entrypoints, stale state, duplicate
canonical claims, orphaned scratch, expired logs and missing handoff promotion.
It may delete only manifest-owned scratch under explicit cleanup authority.
Skills above the canonical whole-file entrypoint budget, including YAML
frontmatter, move stable detail to targeted references; core rules remain early
in the entrypoint.

## Evaluation strategy

Maintain three suites:

1. contract tests for deterministic invariants;
2. balanced routing/discipline evals with positive, negative and boundary cases
   for every lifecycle/core skill; and
3. independently authored held-out positive, negative and boundary scenarios
   across software, research, analysis, documents, agent products and the
   high-stakes overlay.

Stochastic routing evals run multiple blind batch invocations on declared
model/harness versions. Each invocation retains its input digest, actual model
lineage and hash-bound parsed output; case selections reference that invocation.
Report confidence intervals or raw numerator/denominator, not a single opaque
score. Production/session examples enter a shared or exported dataset only
after disclosure review and explicit approval. Capability cases and regression
cases are labelled separately.

## Required delivery sequence

| Phase | Required outcome |
|---|---|
| 1 — evidence foundations | Evidence contracts and tests |
| 2 — neutral kernel | One public v1 contract for every profile |
| 3 — bound gates | Digest-bound authority, security and observation gates |
| 4 — managed evolution | Reconciled installation and routing evidence |
| 5 — prove and accept | Deterministic proof, independent review and explicit user acceptance |

### Phase 1 — evidence foundations

- Make `skill-craft`'s audit branch static-first with separate local and shared/export evidence
  modes. Use a deterministic contract and routing fixture; do not claim native
  provider telemetry without real adapters and producers.
- Add `skills/retrospect/templates/RETROSPECT.template.json`, a validator and pass/fail fixtures.
- Record the research baseline and this specification.

### Phase 2 — neutral kernel

- Add the neutral run schema, profile registry and kernel validator.
- Add software, research, analysis, document and agent-product profiles plus
  orthogonal high-stakes safeguards.
- Make `implement` emit the canonical software-profile receipt and remove its
  superseded receipt shape and validator.
- Establish the routing baseline before deciding whether to expose `deliver`
  as a new entry skill.

### Phase 3 — bound gates

- Add design artifact/digest/approver validation.
- Add security evidence selection and observation contracts.
- Connect incident and recurrence evidence.

### Phase 4 — managed evolution

- Add installation ownership/reconciliation.
- Unify precedence wording.
- Add trigger fixtures and cross-domain held-out evals.
- Compact oversized skills and implement manifest-led run cleanup.

### Phase 5 — prove and accept

- Run regression and public-safety checks.
- Exercise all profiles with reference runs and negative cases.
- Obtain independent targeted, other-primary and available distinct-family review.
- Repair, record remaining degradation and request final user acceptance.

## Stability and rollback

- `implement` remains directly triggerable and uses the canonical software
  profile receipt.
- `delivery-run` has one schema. Breaking changes require an explicit design
  decision; the harness does not carry unused compatibility adapters.
- Each phase is independently revertible; no migration deletes existing run
  evidence or installed skills.
- If a profile cannot prove its gates, it falls back to the existing specialised
  skill and records `kernel_degraded`, never fabricates completion.

## Acceptance criteria

The refactor is complete when:

1. Reference runs for all five profiles pass the neutral validator and preserve
   outcome/trajectory evidence.
2. `implement` uses the canonical software profile and `release` consumes only
   accepted canonical delivery receipts.
3. Design approval without artifact/digest/approver fails.
4. Requested local-history analysis proceeds read-only without a second privacy
   gate; shared/export output requires destination and content authority;
   unsupported attribution remains `N/A`.
5. A retrospective without baseline/comparator reasons, evidence-linked root
   causes or recurrence state fails.
6. Crucial software and agent-product runs cannot close without applicable
   security evidence and independent primary-family review.
7. Observation has a window, signals, thresholds, owner and containment path.
8. Installer dry-run distinguishes managed, unmanaged, stale and conflicting
   targets and never overwrites unmanaged content.
9. All core skills have balanced trigger fixtures; held-out profile runs meet
   declared thresholds across repeated trials.
10. Public-safety, deterministic harness tests, clean-install tests and context
    budgets pass.
11. The other-primary reviewer and fresh targeted reviewers independently report
    no unresolved blocking findings.
12. The user accepts the completed lifecycle; release remains a separate gate.

### Implementation evidence

| Criterion | Evidence |
|---|---|
| 1–3 | `config/delivery-profiles.json`, `skills/deliver/`, `tests/test_delivery_contract.py` |
| 4 | `skills/skill-craft/SKILL.md`, `skills/skill-craft/references/audit.md`, `skills/skill-craft/references/method.md`, local-history routing fixture and `test_skill_audit_contract.py` |
| 5 | `skills/retrospect/templates/RETROSPECT.template.json`, `validate_retrospect.py`, adversarial receipt tests |
| 6 | `config/security-evidence.json`, security selector and crucial-gate tests |
| 7 | typed observation contract and strengthened `RELEASE.json` observation gate |
| 8 | `manage_installation.py`, rename registry and managed-install tests |
| 9 | balanced core fixtures, held-out dataset and repeated Fable routing receipts |
| 10 | `scripts/check-harness`, public-release check, clean-install and context-budget suites |
| 11 | HREF-002 targeted and other-primary review artifacts after the final source freeze |
| 12 | Explicit user acceptance evidence; no release or push authority is implied |

## Known risks and controls

| Risk | Control |
|---|---|
| Kernel becomes a bloated mega-skill | Stable state machine only; profiles and references hold depth; word-budget test. |
| Generic gates weaken domain requirements | Profiles may strengthen only; high-stakes release is always user action. |
| Receipt ceremony overwhelms small tasks | Risk threshold; routine one-shot work may use an ephemeral receipt or none. |
| Local history escapes its requested scope | Read in place, never commit raw history and keep sharing/export behind explicit destination and content authority. |
| Evals optimise to their own fixtures | Held-out cases, repeated trials, mixed graders and user calibration. |
| Multi-agent cost exceeds value | Decomposability gate, one writer and proportional lanes. |
| Concurrent agent-fabric work conflicts | Unique files in Phase 1; shared entrypoints deferred until fabric ownership closes. |
| Research becomes stale | Dated evidence cut-off, 90-day decision expiry and retrospective refresh proposal. |

## User authority boundaries

The held-out baseline justifies `deliver`; the instruction to implement this
approved specification entirely selects that named entrypoint. Non-trivial
neutral runs persist receipts; tiny routine one-shot work remains exempt.
Requested local history is read in place and not retained as project truth.
Sharing or exporting derived evidence requires a separate user decision.
Lifecycle acceptance, runtime activation, live installation, provider login,
push and release each require separate user authority.

## Route and topology evaluation evidence

The mature route-evaluation findings from the
[July 2026 continuity and routing snapshot](../../research/evidence-snapshots/agent-continuity-routing-2026-07.md)
extend the neutral delivery evidence contract. They do not authorise a learned
router, a global model leaderboard or automatic preference mutation.

When a model route or multi-agent topology materially affects an outcome or
review gate, the run registers one or more closed
`routeEvaluationEvidenceV1` JSON artifacts. `RUN.json.artifacts[]` contains the
ordinary delivery artifact row for each payload. `RUN.json.evidence[]` contains
only an ordinary base evidence row—never the payload object itself:

```yaml
id: stable-evidence-id
kind: deterministic
gate: route-evaluation-contract
method: adaptive-harness-route-evaluation.v1
status: pass | fail | unavailable | not_applicable
artifact_id: exact-RUN-artifact-id-for-route-evaluation-payload
source_paths: [authority-scoped-input-paths]
result:
  exit_code: integer
  receipt_digest: exact-payload-artifact-digest
```

The base row retains its existing required field set and validation semantics;
no route-specific sibling or nested extension is added to `delivery-run`.
Judgement aggregate rows remain ordinary judgement evidence with their existing
`model_lineage`. The registered payload carries the route-specific contract.
All payload fields are required; nullable values represent unavailable
measurements rather than omitted truth.

```yaml
evaluatedRouteIdentityV1:
  schemaVersion: 1
  hostId: exact-host
  hostVersion: exact-host-version
  adapterId: exact-adapter
  adapterContractDigest: sha256-prefixed-digest
  endpointProvider: exact-provider
  family: canonical-family
  model: exact-model
  resolvedEffort: resolvedEffortV1
  normalizedReasoningEffort: none | low | medium | high | xhigh | max | null
  rawNativeMode: exact-provider-value | null
  orchestrationMode: single | native-subagents | dynamic-workflow | provider-multi-agent
  capabilityBodyDigest: sha256-prefixed-digest
  requestedConfigurationDigest: sha256-prefixed-digest
  effectiveConfigurationDigest: sha256-prefixed-digest
  permissionProfileDigest: sha256-prefixed-digest
  discoverySurfaceRef: discoverySurfaceRefV1
  routePolicyRevision: exact-revision
  harnessRevision: exact-revision
  harnessDigest: sha256-prefixed-digest
  contextPolicyRevision: exact-revision
  contextPolicyDigest: sha256-prefixed-digest
  topologyWavePlanRef: topologyWavePlanRefV1

routeEvaluationEvidenceV1:
  schemaVersion: 1
  taskClass: stable-task-class
  evaluatedRouteIdentity: evaluatedRouteIdentityV1
  evaluatedRouteIdentityDigest: sha256-prefixed-digest
  evaluationPlanRef: registeredEvidenceRefV1
  plannedTrialCount: positive-integer-at-most-256
  trialRoutes:
    - ordinal: contiguous-positive-integer
      actionRef: ProviderActionRefV1
      deployedRouteAdmissionDigest: sha256-prefixed-digest
      deployedRouteObservationDigest: sha256-prefixed-digest | null
  topologyWavePlanRef: topologyWavePlanRefV1
  harnessRevision: exact-revision
  harnessDigest: sha256-prefixed-digest
  discoverySurfaceRef: discoverySurfaceRefV1
  routePolicyRevision: exact-revision
  contextPolicyRevision: exact-revision
  contextPolicyDigest: sha256-prefixed-digest
  datasetDigest: sha256-prefixed-digest
  trialCount: positive-integer
  objectivePassCount: nonnegative-integer | null
  objectiveTrialCount: positive-integer | null
  judgementAggregateRef: registeredEvidenceRefV1 | null
  reliabilityAggregateRef: registeredEvidenceRefV1 | null
  efficiencyAggregateRef: registeredEvidenceRefV1 | null
  baseline:
    kind: best-single | cheapest-acceptable | prior-policy | simple-single-owner | none
    evidenceRef: registeredEvidenceRefV1 | null
    absenceReason: exact-safe-reason | null
  observedAt: timestamp
  expiresAt: timestamp
  promotionState: bootstrap | shadow | advisory | canary | task-class-active | expired
  evidenceDigest: sha256-prefixed-digest
```

`registeredEvidenceRefV1` is not a new artifact-reference type. It is schema
shorthand for the exact existing the Agent Fabric contract `EvidenceArtifactRegistration` tuple:
`{evidenceId, evidenceRevision, artifactRef:{path,digest}}`, equality-bound to
the same current run/session and immutable registration revision. The generated
schema expands that tuple directly.

The payload, `evaluatedRouteIdentityV1`, nested baseline and every registration
reference are closed.
`objectivePassCount` and
`objectiveTrialCount` are both null or both non-null, and the count cannot
exceed the denominator. A non-null denominator cannot exceed the number of
distinct trial rows with a non-null, parent-bound proved observation. Baseline
`none` requires a non-null reason and null reference; every other baseline
requires a reference and null reason.
`evidenceDigest` is SHA-256 of RFC 8785 JCS over the complete record with only
that field omitted.

`evaluatedRouteIdentityDigest` is SHA-256 of RFC 8785 JCS of exactly the closed
`evaluatedRouteIdentityV1` object as displayed, with no omitted or added field.
The identity is action-free: it excludes action IDs, snapshot instance/clocks,
trial ordinals and observations while retaining the stable capability body,
effective configuration, permission, discovery, route/harness/context policy
and exact topology-wave
row identity. The payload's top-level harness, route-policy,
context-policy, discovery-surface and topology-wave refs equality-copy that
preimage; a mismatch rejects.

`evaluationPlanRef` names the current immutable run-owned evaluation plan and
its exact revision/digest; that plan declares this task class, route-evaluation
kind, dataset, baseline and `plannedTrialCount`; those values must equal this
record. The protocol safety maximum is
256; it is not a recommendation for policy volume. `trialRoutes` is nonempty;
ordinals are contiguous, canonical `(adapterId,actionId)` pairs are distinct,
admission digests are distinct
and array length equals both `trialCount` and `plannedTrialCount`. Each
action/admission/optional-observation tuple equality-resolves through the
current Fabric receipt/evidence registration. After removing action and
snapshot-instance identity, every admission must reproduce the route-owned
fields of the displayed `evaluatedRouteIdentityV1`; its capability-body digest
permits instance refresh without conflating different capability content. The
evaluation plan supplies and equality-binds the topology-wave ref. The top-level discovery-surface registration
must equal every admission's `discoverySurfaceRefV1` evidence ID/revision/path/
digest. Top-level harness, route-policy and context-policy revisions/digests
must likewise equal every admission; the topology wave must equal the plan. A
different value rejects. A non-null observation digest
must parent-bind its trial admission. Null means the trial has no proved
terminal observation and cannot contribute an objective pass.

Evidence is scoped to the deployed unit—host, adapter/contract, model, raw
effort, native mode, harness revision, discovery surface, topology and context
policy—not to a model name alone. Stochastic comparisons use repeated trials;
deterministic cases retain exact numerator/denominator. A changed harness,
adapter contract, route policy, discovery surface, dataset or expired record is
not current route evidence. Capability and safety constraints remain hard gates
regardless of an evaluation result.

Every topology-bearing evaluation references the exact Agent Fabric contract
`topologyWavePlanV1` row through the closed the Agent Fabric contract
`topologyWavePlanRefV1`; the ref equality-binds session/run/task/wave/revision/
digest to the current or historical plan row. That plan already owns
dependency/decomposability, topology, contention, one accountable chair, stage
owners, write partitions, budget, stop conditions, authority/policy lineage and
append-only rationale. The evaluation cannot restate or broaden them.
Parallelism is evidence-driven and bounded; agent count is not a quality
measure.

Promotion is task-local. A new deployed route moves through bootstrap,
shadow/advisory and canary evidence before `task-class-active`; expiry returns
it to explicit stale/bootstrap handling. This contract records the evidence
and promotion state only. Candidate-pool construction, Pareto elimination,
quality-floor values, trial volumes, expiry intervals and any learned selector
remain future policy decisions.

Portable aggregates may include counts, latency buckets, token/cost values and
classified failure codes. They exclude prompts, answers, tool arguments,
artifact content, private messages, secrets, project names and absolute paths.
Rich local evidence stays run-owned and is referenced by digest. Validation
fixtures cover the conforming base evidence row/artifact join, exact identity
preimage and digest, distinct action pairs/admission digests, planned/effective
trial equality and numerator/denominator bounds, baseline nullability, expiry
and revision drift, task-class isolation, topology-wave currency, content-free
export and the absence of any automatic promotion side effect.
