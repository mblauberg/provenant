# Adaptive agent harness lifecycle

The repository's `Repository process` declaration names the canonical
scope/story home. This specification grants no authority. Current user and
project instructions, plus an authorised run, are required; only an authorised
run may make scoped reversible changes. The repository declaration points to
the current scope/story and workflow-state owners; this file retains only the
durable lifecycle requirements.

The current contract permits direct read-only analysis of local session history,
keeps sharing and export separately gated, and treats unsupported or
unattributable evidence as `N/A`, never zero. It does not retain the synthetic
collector, which had no provider-native adapter or producer. Route evaluation
remains task-local, receipt-bound and content-free; it does not approve a
learned or Pareto router.

## Authority and decision

The lifecycle contract grants nothing. An authorised run may make scoped
reversible repository changes, tests and documentation within the existing
harness. It does not authorise provider login, external communications,
deployment, live installation, destructive migration, Git push or release.

This specification is grounded in
[`docs/research/agentic-sdlc-harness-2026.md`](../../research/agentic-sdlc-harness-2026.md).
Research claims expire for decision purposes after 90 days or when a cited
standard/provider interface materially changes, whichever occurs first.

## Historical problem

At adoption, the repository had a mature software loop but no shared executable
contract for non-code delivery. Design approval could be unbound from its
artifact, and security, observation, installation reconciliation and trigger
evaluation lacked sufficient machine evidence.

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
- Reimplementing the current daemonless SQLite Fabric bus.

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

The delivery kernel is a domain-neutral contract and validator. It orchestrates
existing capabilities; it does not contain domain expertise. `RUN.json` is a
flat record of what a run did, checked for shape and for the evidence each
recorded gate demands. There is no transition graph: a receipt written by the
agent that performed the run cannot certify its own ordering.

#### Recorded gates

The gates a receipt records, each an independent flat fact:

- `intent.approval` and `design.status`, each bound to a digested artifact and
  to passing human approval evidence.
- `authority`, bound to passing human authority-approval evidence.
- `human_gates.acceptance`, which requires the profile's deterministic and
  judgement evidence, the review ladder, the security surfaces and the measures.
- `human_gates.release`, which requires acceptance.
- `observation`, which a released run must have active or passing, and a closed
  run must have passing inside its declared window.
- `retrospective`, required for a closed run by risk, incident, escaped defect
  or repeated-correction policy.

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
triggerable. The `deliver` entrypoint calls only what the
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

Use a versioned installation manifest containing skill name, source digest,
installed target and current ownership. Installer operations support `plan`,
`install`, `reconcile` and `uninstall-managed`. Unmanaged existing paths are
never claimed or overwritten. Broken managed links and safe managed retirements
are reconciled. The target-bound manifest hashes full skill-tree bytes and
executable modes, then is written atomically after link reconciliation.
Conflicts stop for user resolution.

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
Skills above the body budget move stable detail to targeted references; core
rules remain early in the entrypoint.

## Evaluation strategy

Use the least costly evidence that answers the question:

- deterministic contract and fixture tests own lifecycle and machine invariants;
- balanced positive, negative and boundary fixtures remain required for core
  skills; and
- live/provider semantic held-outs run only when ADR 0014 or `MAINTAINING.md`
  triggers them: a suspected or observed routing regression, a multi-skill
  trigger rewrite at maintainer discretion, or preparation for publication or
  another operator.

When an evaluation runs, record the actual route and model, plus raw
numerator/denominator for each result. No universal hash or version-lock
protocol is required; use existing receipt digests and revisions only where
their owning contract requires them. Production or session examples enter
shared/exported datasets only after disclosure review and explicit approval.
Label capability and regression cases separately.

## Stability and rollback

- `implement` remains directly triggerable and uses the canonical software
  profile receipt.
- `delivery-run` has one schema. Breaking changes require an explicit design
  decision; the harness does not carry unused compatibility adapters.
- Each contract change is independently revertible; no migration deletes
  existing run evidence or installed skills.
- If a profile cannot prove its gates, it falls back to the existing specialised
  skill and records `kernel_degraded`, never fabricates completion.

## Acceptance criteria

The lifecycle requirements remain satisfied when:

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
9. Core skills have balanced trigger fixtures; conditional held-out evaluations
   record the actual route/model and raw numerator/denominator.
10. Public-safety, deterministic harness tests, clean-install tests and context
    budgets pass.
11. The other-primary reviewer and fresh targeted reviewers independently report
    no unresolved blocking findings.
12. The user accepts the lifecycle outcome; release remains a separate gate.

## Known risks and controls

| Risk | Control |
|---|---|
| Kernel becomes a bloated mega-skill | Stable state machine only; profiles and references hold depth. |
| Generic gates weaken domain requirements | Profiles may strengthen only; high-stakes release is always user action. |
| Receipt ceremony overwhelms small tasks | Risk threshold; routine one-shot work may use an ephemeral receipt or none. |
| Local history escapes its requested scope | Read in place, never commit raw history and keep sharing/export behind explicit destination and content authority. |
| Evals optimise to their own fixtures | Conditional held-outs, repeated trials, mixed graders and user calibration. |
| Multi-agent cost exceeds value | Decomposability gate, one writer and proportional lanes. |
| Concurrent agent-fabric work conflicts | Fabric coordinates; one writer and explicit write partitions protect shared surfaces. |
| Research becomes stale | Dated evidence cut-off, 90-day decision expiry and retrospective refresh proposal. |

## User authority boundaries

An issue or receipt records scope or evidence; it is not approval. Current user
and project instructions control. External release, promotion, activation,
sharing/export, provider login, live installation, push and destructive action
remain separately gated by current authority.

## Route and topology evidence boundary

Dispatch and batch receipts own the actual resolved route, provider/model and
attempt facts. Delivery may reference those receipts for evidence. Fabric MCP
may start the existing dispatch owners, but does not implement provider
mechanics or own lifecycle acceptance.
Assurance labels reflect the evidence that exists and do not infer coverage.

This specification defines no learned router, global leaderboard, automatic
promotion, or universal digest/version protocol. Route or topology comparison
is task-local and opt-in under the evaluation triggers above; the owning
dispatch or batch receipt remains the source for what actually ran.
