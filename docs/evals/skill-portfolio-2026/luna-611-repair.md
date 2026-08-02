# Issue #611 repair evidence

Date: 2026-08-02
Worktree: `$AGENTS_HOME/.worktrees/eval-result-contract`
Scope: sole-writer repair of the existing uncommitted validator/test diff

## Result

The current fixture remains `planned-unexecuted`. No provider action, evaluation
engine, registry, generic schema framework, or runtime service was added.

The future `completed` contract now requires:

- the exact completed execution shape and the frozen schedule's six attempts;
- exactly 108 ordered case-result rows covering every attempt and holdout case;
- terminal attempt and case accounting derived from retained rows;
- primary accuracy, companion fidelity, and critical-case metrics derived from
  retained correctness fields;
- non-success attempt states matched to their non-semantic case-row states;
- requested and actual adapter/family/model/effort lineage on every attempt;
- explicit substitution reason and disposition when actual lineage differs;
- unique route and receipt identities whose retained artifact digest and bytes
  bind the exact attempt lineage; and
- summary attempts, cases, owners, dependencies, and status bound to the
  execution/result evidence.

Planned state retains the exact dependency list and rejects result evidence.
Protocol and source digests remain immutable and cannot be rebound by changing
the checked-in plan or holdout and recomputing a result hash.

## Adversarial coverage

`tests/test_skill_eval_fixtures.py` covers the supported findings from both
Luna reports, including:

- partial execution, missing attempts, missing case rows, extra execution
  fields, and planned dependency removal/replacement/mismatch;
- summary case, owner, dependency, and digest drift;
- missing or mutated actual lineage, undeclared substitution, lineage/receipt
  drift, absent or digest-mismatched route receipts, and planned-lineage echo;
- accounting and metric drift; and
- omitted-row correctness and attempt/case terminal-state drift.

## Verification evidence

- `./.venv/bin/pytest -q tests/test_skill_eval_fixtures.py` -> `34 passed`.
- `./.venv/bin/pytest -q tests/test_skill_route_evaluation.py tests/test_skill_eval_fixtures.py` -> `44 passed`.
- `./.venv/bin/python -m compileall -q scripts/validate_skill_routing_evaluation.py tests/test_skill_eval_fixtures.py` -> pass.
- `./scripts/validate_skill_routing_evaluation.py docs/evals/skill-reuse-2026/receipt.json` -> `PASS: exact candidate-bound routing 29/30; cross-primary-family promotion gate unmet`.
- `git diff --check` -> pass.
- Final independent GPT-5.6 Luna review -> `CLEAN`; no high-confidence actionable findings remained.
- No live provider execution was attempted.
