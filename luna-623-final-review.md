# Issue #623 final review

## Verdict

The #623 structured change-gate repair is complete within scope. The final independent Luna review found no remaining code defect. Its `NOT CLEAN` status was solely because this required untracked review artifact was absent while it inspected the worktree; the artifact is now restored before commit.

## Diagnosis

The historical run in `/tmp/provenant-623-mutation-output2.EWQWmQ` reported `mutants=14 killed=9 survivors=5` and labelled the five non-killed cases `INVALID`. They were supported equality/inequality mutations at `scripts/change_gate_reports.py:110,131,135` and `scripts/change_gates.py:335,338`. Their test commands executed, but the old gate collapsed non-assertion outcomes into `INVALID` and hard-coded `inconclusive=0`. This was a harness classification defect, not unsupported mutation operators.

## Repair

`gate_changed_line_mutation` now classifies executed mutants as `KILLED`, `SURVIVED`, or `INCONCLUSIVE`. A crucial or terminal gate fails for either survivors or inconclusive evidence, and the summary reports both counts. No mutation operator or mutation framework expansion was added.

The Vitest parser keeps a failed file-level error separate from assertion-record markers, while ignoring an error field attached to a passed assertion record. This preserves mixed evidence without allowing a test-body marker to establish import identity.

The type-only grammar now accepts only wholly type-only named imports/exports. The adversarial case `import { type Foo, runtime } from './m';` is rejected as runtime/mixed and cannot reach the type-only bypass.

The type-only route provides explicit no-op ownership. Its revert output is:

```text
REVERT_PROBE: PASS owner=type-gate hunks=1 killed=0 survivors=0 inconclusive=0
```

The focused regression asserts that exact zero/zero line.

## Verification

All commands below were run against the current worktree.

1. Issue-scoped global mutation gate, based on `HEAD`:

   ```text
   .venv/bin/python scripts/change_gates.py changed-lines-only \
     --base HEAD --source-root . \
     --scratch-root /tmp/provenant-623-final-mutation7.<pid>.tmp \
     --test-command-py '.venv/bin/python -m pytest {test} -q' \
     --test-command-ts 'npm exec vitest run {test}' \
     --test tests/test_change_gate_reason_modes.py \
     --risk crucial --mode behaviour
   ```

   Result: `CHANGED_LINES_MUTATION: PASS mutants=20 killed=20 survivors=0 inconclusive=0`.

   The 20 points include covered classification and mode branches introduced by the repair. All were killed; no operator was added.

2. Focused mode tests: `26 passed`.

3. Focused four-file gate suite:

   ```text
   tests/test_change_gate_reason_modes.py
   tests/test_change_gate_reports.py
   tests/test_change_gate_runner.py
   tests/test_change_gates.py
   ```

   Result: `117 passed`.

4. Full relevant gate and workflow-policy suite, including `tests/test_ci_repository_assurance_policy.py`: `153 passed`.

5. `git diff --check`: passed.

6. Python compilation for the three changed gate modules: passed.

7. The live right-reason-red command against `HEAD` passed with expected new-target evidence: `RIGHT_REASON_RED: PASS tests=1 assertion=0 new-target=1`.

## Independent reviews

- Fresh `gpt-5.6-luna` review, agent `019fbf13-fe3c-7633-a1ad-d465980c0fe5`, returned `CLEAN` and confirmed the historical `INVALID` diagnosis, repaired classification, 16/16 mutation result at that stage, and exact type-only no-op.
- Fresh final-state `gpt-5.6-luna` review, agent `019fbf22-8375-71f2-9de7-689231490cd9`, found and blocked the mixed named-import defect. That defect was repaired with the narrow grammar change and regression above.
- Final fresh `gpt-5.6-luna` review, agent `019fbf26-c7b9-7b40-86b8-0c3c465557bf`, independently confirmed the corrected mixed-import classification, `153 passed`, the `20/20` mutation result, exact zero/zero type-only revert, and `git diff --check`. It reported `NOT CLEAN` only because this artifact was absent during inspection, not because of a code finding.

## Scope

Only the issue-scoped implementation, workflow declaration, tests, and this review artifact are changed. No push, merge, other worktree, or mutation-framework expansion was performed.
