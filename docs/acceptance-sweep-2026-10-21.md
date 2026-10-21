# Acceptance sweep: 21 October 2026

Route: `codex/gpt-6.1-sol@high` via Fabric. Scope: documentation, policy
metadata and eval fixtures in `.worktrees/docs-acceptance-sweep`; commit only,
no push. `HARNESS.md` and `AGENTS.md` belong to the chair's separate rewrite.

## Rule

The owner directed:

> I don't want to have to be reviewing as much as the harness has said prior,
> because tests and agent reviewers are good enough, particularly since I am
> usually there at the start providing direction, scoping, and more.

Objective checks plus the review required by the tier meet delivery acceptance.
The chair reports the outcome and the user may still reject it. Explicit user
acceptance is required only for `terminal` work or when the approved scope asks
for it. A project may name its landing gate as acceptance; Sealle names landing
as acceptance for everything.

User approval remains at specs and scope, one-way doors, risk-tier downgrades,
unresolved acceptance criteria, destructive or irreversible actions, external
communications and production promotion. Acceptance grants no release, deploy,
publish or send authority.

## Changed files

The sweep covered all tracked skills, references, eval fixtures, configs and
docs, including hidden GitHub issue templates. Searches covered acceptance,
human/user approval and review, sign-off, awaiting-human and unaccepted-delivery
wording. Every changed file follows; this report is the additional sweep artefact.

### Lifecycle skills

Use checks and tier-required review for acceptance; preserve terminal/scoped user gates and separate promotion authority.

- `skills/autopilot/SKILL.md`
- `skills/deliver/SKILL.md`
- `skills/evaluate/SKILL.md`
- `skills/implement/SKILL.md`
- `skills/orchestrate/SKILL.md`
- `skills/release/SKILL.md`
- `skills/scope/SKILL.md`
- `skills/tdd/SKILL.md`
- `skills/ui-ux-design/SKILL.md`

### Skill references and templates

Align role boundaries, unattended completion, tracker states and planning language. Receipt references explicitly identify the retained executable constraint.

- `skills/autopilot/templates/README.template.md`
- `skills/deliver/references/contract.md`
- `skills/engineering-writing/references/requirements-and-planning.md`
- `skills/evaluate/references/receipt.md`
- `skills/implement/references/run-contract.md`
- `skills/orchestrate/references/autonomous-implementation.md`
- `skills/orchestrate/references/evaluation-and-observability.md`
- `skills/orchestrate/references/orchestration-contract.md`
- `skills/orchestrate/references/paired-primary.md`
- `skills/setup-repo/references/doctrine.md`
- `skills/setup-repo/references/project-board.md`
- `skills/setup-repo/templates/ISSUE_TEMPLATE/work-item.yml`
- `skills/ui-ux-design/references/visual-qa.md`

### Active eval fixtures

Replace the ordinary final human gate; cover terminal, scoped explicit acceptance and project landing. Expected skill routes remain unchanged.

- `docs/evals/skill-portfolio-2026/routing-holdout.yaml`
- `skills/deliver/evals/lifecycle_cases.yaml`
- `skills/deliver/evals/trigger_cases.yaml`
- `skills/evaluate/evals/trigger_cases.yaml`
- `skills/implement/evals/trigger_cases.yaml`
- `skills/orchestrate/evals/trigger_cases.yaml`
- `skills/release/evals/lifecycle_cases.yaml`
- `skills/release/evals/trigger_cases.yaml`
- `skills/scope/evals/trigger_cases.yaml`

### Policy and project documentation

Align diagrams, operating requirements, status meanings and policy metadata. Historical research and the accepted ADR retain their original text with a dated current-policy note.

- `.github/ISSUE_TEMPLATE/work-item.yml`
- `README.md`
- `config/risk-policy.json`
- `docs/ARCHITECTURE.md`
- `docs/adr/0005-lifecycle-kernel-extends-delivery.md`
- `docs/evals/skill-portfolio-2026/README.md`
- `docs/glossary.md`
- `docs/research/agentic-sdlc-harness-2026.md`
- `docs/research/lifecycle-routing-baseline-2026-07-10.md`
- `docs/research/skill-portfolio-practices-2026.md`
- `docs/runbooks/github-workflow.md`
- `docs/specs/fabric-v2.md`
- `docs/specs/harness/lifecycle.md`
- `docs/specs/harness/operating-model.md`

`config/risk-policy.json.acceptance_policy` records the normative rule. The
existing risk-factor mappings and schema version are unchanged. The loader at
`skills/_shared/delivery_run_invariants.py:86` still consumes tier order and
factors; this metadata does not change an executable acceptance gate.

## Intentionally kept

| Files or sites | Reason |
|---|---|
| `HARNESS.md`, `AGENTS.md` | Explicitly excluded; the chair owns the constitutional rewrite. Their old wording is not an endorsement of the old rule. |
| `skills/release/SKILL.md` entry, readiness, promote and user-gate sections; `skills/release/templates/RELEASE.template.json` | Exact artifact, action, target, expiry, disclosure, reversal and explicit promotion authority remain required. Only the source of delivery acceptance changed in prose. |
| `config/delivery-profiles.json:104`, `:106`, `:107` | Source-authority evidence, qualified-domain review and explicit high-stakes action approval are distinct safeguards. The profile schema is closed; no new acceptance field was added here. |
| `skills/deliver/templates/RUN.template.json:99`; delivery producer and validator fixtures/tests | Preserve legacy `human_gates.acceptance` and `release` fields and matching human evidence until a separately tested code migration. |
| All `skills/*/portable-workflow.v1.json` | `accepted_artifact_identity` pins identity context; it is not a demand for final user approval. |
| `evals/lifecycle-routing.yaml`; `docs/evals/skill-portfolio-2026/predecessor/routing-holdout-20260714.yaml`; `predecessor/routing-result-20260714.json`; predecessor records in `docs/evals/skill-portfolio-2026/summary.json` | Frozen historical prompts/results describe their recorded policy and catalogue. Do not rewrite evidence after execution. The current unexecuted `routing-holdout.yaml` was aligned, and the portfolio README explains the historical boundary. |
| `docs/adr/0005-lifecycle-kernel-extends-delivery.md`; the three changed research baselines | Historical conclusions remain intact under a dated policy qualification. |
| `skills/ui-ux-design/references/live.md` and live accept scripts | Preview selection/saving is a separate interactive operation, not final lifecycle acceptance. |
| `skills/legal-writing/SKILL.md` and references | Qualified legal review, filing-facing checks, correspondence sign-offs and explicit send/file gates are domain safeguards, not a universal delivery gate. |
| `skills/autopilot/references/operating-loop.md`; `templates/HANDOFF.template.md`; build-ceiling sign-off in `templates/README.template.md` | STOP, expert authority, one-way doors and a user-approved build ceiling remain gates. |
| `skills/orchestrate/references/dynamic-workflows.md`; `codex-subagents.md`; `herdr-panes.md` | Gate-adjacent runtime stops and external-action authority remain valid for actual user gates. |
| Other skills, references, configs, docs and fixtures without a final user-acceptance requirement | Observable acceptance criteria, user stories, approved scope, third-party registry/recipient acceptance and historical accepted-decision status require no policy rewrite. |

## Code sites: unchanged, recommendations only

These are executable or compatibility constraints, not permissions to fabricate
user approval. Recommendations need a separate behaviour change with TDD.

| File:line | Current role | Recommendation |
|---|---|---|
| `skills/deliver/scripts/delivery_run_shape.py:152` | Acceptance means approved `human_gates.acceptance`. Closed receipt fields/statuses are at `:21` and `:66`. | Add a compatible, evidence-bound delivery-acceptance predicate that distinguishes checks/review, project landing and required explicit user approval. |
| `skills/deliver/scripts/validate_delivery.py:93`, `:99`, `:124`, `:130`, `:141`, `:143`, `:147` | Acceptance approval activates deterministic/judgement, review, profile, security, measures and high-stakes assurance. | Activate all completion checks for every acceptance path. Merely setting `not-required` would bypass assurance and is not a fix. |
| `skills/deliver/scripts/delivery_validation_security.py:77`, `:89` | Approved acceptance requires passing human evidence; release requires approved acceptance. | Permit evidence-backed non-human delivery acceptance where allowed. Keep terminal/scoped human approval and separate release authority. |
| `skills/deliver/scripts/delivery_receipt.py:99`, `:474` | Acceptance freezes measures, assurance/security plans and software-delivery bindings. | Freeze on valid delivery acceptance, irrespective of its permitted basis. Preserve immutability. |
| `skills/deliver/scripts/delivery_receipt.py:385`, `:712`, `:785` | Reserves human-acceptance evidence and records an approved human gate from evidence. | Retain explicit human-evidence support for terminal/scoped approval; add a separate truthful producer path for ordinary acceptance. |
| `skills/deliver/scripts/reference_runs.py:72`, `:230` | Reference receipts generate approved human acceptance and matching evidence. | Retain historical examples; add passing/failing cases for each permitted acceptance basis and every retained assurance gate. |
| `skills/implement/scripts/bind_merged_delivery.py:148`, `:225` | Merge binding is refused after human approval; success requests human acceptance. | Bind exact merged evidence before project landing acceptance is recorded, then report the appropriate acceptance basis. Preserve exact-head, tree, CI and authority checks. |
| `scripts/clean.py:161`, `:240` | Only approved/accepted/legacy-boolean human acceptance counts; every other delivery is retained. | Recognise validated delivery acceptance under the new rule. Keep retention windows, live/pinned/cited guards, digest approval and deletion identity checks. |
| `workflows/implement-run.js:22`, `:399`, `:770`, `:774`, `:843`, `:857` | Prompts and success state unconditionally keep acceptance pending and await the user. | Complete ordinary work when objective and required review gates pass; request explicit acceptance only for terminal/scoped work. Preserve apply, external-action and promotion gates at `:755`, `:765`, `:807`. |
| `workflows/codebase-polish.js:543` | Always returns `awaiting-human`, even after already-applied low-risk work. | Derive completion from remaining criteria, escalations, tier and scope; keep genuine user decisions pending. |
| `skills/session/scripts/cleanup_run_artifacts.py:100`; `skills/_shared/run_gates.py:27` | Cleanup requires delivery validation and closed release/observation, indirectly depending on legacy acceptance. | Reuse the aligned validator; retain release/observation, explicit cleanup authority and digest gates. Do not relax closure or deletion authority as part of acceptance alignment. |
| `skills/deliver/scripts/delivery_policy_validation.py:17`, `:86`, `:149` | Delivery profile keys are closed and schema version remains 1. | If adding profile acceptance policy later, explicitly admit/version it and preserve all existing profile safeguards. |
| `scripts/worktree.py:643` | Manual acceptance belongs to `chair-orchestrator` for commit-claim verification. | Keep; this is chair verification, not user acceptance. |

Related code tests remain unchanged: `tests/test_delivery_contract.py`,
`tests/test_delivery_receipt_producer.py`, `tests/test_software_delivery_continuity.py`,
`tests/test_clean.py`, `tests/test_run_artifact_cleanup.py`,
`tests/test_implement_run_workflow.py` and
`skills/implement/tests/test_checkpoint_run.py`.

## Verification and limits

- `uv run --locked --group test pytest -q tests/test_skill_eval_fixtures.py`:
  **9 passed**. Covers active routing-fixture structure and frozen provenance.
- `uv run --locked --group test pytest -q tests/test_projection.py`:
  **16 passed**. Includes risk-table agreement and risk derivation against config.
- Parsed **21** changed JSON/YAML/frontmatter documents; **162** local Markdown
  targets resolved. `git diff --check` passed.
- Both updated Mermaid diagrams were submitted to `mmdc`; Chromium could not
  launch because the macOS sandbox denied `bootstrap_check_in` for its Mach
  rendezvous port. Rendering and visual inspection remain unverified; no
  permission escalation was attempted. Diagram structure received source review.
- Independent native review on `codex/gpt-6.1-sol@high`: policy/authority and sweep
  coverage lenses. A separate read-only audit inventoried executable gates.
  The sweep inventory also received a clean follow-up review.
  Other-primary review is deferred to the chair's aggregate review of this batch;
  no cross-family review is claimed for this worker lane.
- No runtime behaviour changed; TDD red/green and receipt/cleanup behaviour tests
  were not invoked. Full suites, `scripts/check-harness` and provider-backed
  semantic routing evals were skipped under the local-check restriction and
  remain CI/chair work. No push, release, deploy, publish or send occurred.
- No `common.md` was supplied below the task or found in this worktree. The
  explicit task rules and supplied `AGENTS.md` instructions were applied.

No scope question blocks this sweep. The chair owns the constitutional rewrite,
CI/aggregate review, and any later code-enforcement migration listed above.
