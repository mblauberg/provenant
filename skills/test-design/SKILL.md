---
name: test-design
description: "Use for TDD and choosing, keeping, merging or deleting tests: acceptance, property, contract, golden and end-to-end. Not for behaviour-preserving refactors or root cause; use refactor or diagnose."
---

# Test design

Choose evidence for observable behaviour and keep the suite economical. For a bug or core logic, write the spec-derived failing test first and witness the reported failure. For substantial work, have an independent author derive acceptance tests from the approved spec. Deleting and trimming tests is normal work. When a test fails on your change, decide whether it caught a defect or only pins the implementation: fix the code in the first case, and rewrite or delete the test in the second. Do not loosen an assertion that encodes a spec clause your change breaks; raise the conflict with the scope owner. Report every removed or weakened test and its reason.

## Choose and prune

1. Make illegal states unrepresentable where possible. Do not test states excluded by types, schemas or lint; identify the enforcing check.
2. Use one contract test per public behaviour, property tests for invariants, golden tests for stable wire formats, and end-to-end tests for a few real paths. Choose the lowest boundary that proves the behaviour.
3. Search existing tests first and extend one when appropriate. Budget at most one new test per new behaviour; name the plausible defect only it catches.
4. On touch, delete duplicates, type-impossible cases and prose assertions; merge overlapping cases and rewrite tests coupled to implementation at the public boundary. A test that fails on a pure refactor needs repair.
5. Mock only process, network, clock and randomness edges. Use real owned collaborators. See [boundaries](references/boundaries.md) and [judgement contrasts](references/judgement.md).

Apply the safeguards in [judgement](references/judgement.md) whenever you add, keep or prune tests.

Run focused tests locally and the project's exact-head CI gates. For risky logic, run mutation testing on the changed surface and investigate survivors. Report net tests added and removed in the PR, and identify unverified evidence.

## How it runs

For substantial work, use `generator-evaluator` with `--set 'parameters.lenses=[{name="tests"}]'` for an independent test review. Its lens reviews the writer's commit; it does not write acceptance tests. Arrange an independent acceptance author separately.

For pruning, classify tests with `explore` over crates, then use `batch` with disjoint writer scopes into one integration branch. Inspect `provenant run --use batch --file BRIEF --explain` before submission.
