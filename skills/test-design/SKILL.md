---
name: test-design
description: "Use for TDD and choosing, keeping, merging or deleting tests: acceptance, property, contract, golden and end-to-end. Not for behaviour-preserving refactors or root cause; use refactor or diagnose."
---

# Test design

Choose evidence for observable behaviour and keep the suite economical. For a bug or core logic, write the spec-derived failing test first and witness the reported failure. For substantial work, have an independent author derive acceptance tests from the approved spec. The implementer must not edit a failing acceptance test to make it pass; resolve a spec conflict with the scope owner. Report every weakened or removed test and its reason.

## Choose and prune

1. Make illegal states unrepresentable where possible. Do not test states excluded by types, schemas or lint; identify the enforcing check.
2. Use one contract test per public behaviour, property tests for invariants, golden tests for stable wire formats, and end-to-end tests for a few real paths. Choose the lowest boundary that proves the behaviour.
3. Search existing tests first and extend one when appropriate. Budget at most one new test per new behaviour; name the plausible defect only it catches.
4. On touch, delete duplicates, type-impossible cases and prose assertions; merge overlapping cases and rewrite tests coupled to implementation at the public boundary. A test that fails on a pure refactor needs repair.
5. Mock only process, network, clock and randomness edges. Use real owned collaborators. See [boundaries](references/boundaries.md) and [judgement contrasts](references/judgement.md).

Run focused tests locally and the project's exact-head CI gates. For risky logic, run mutation testing on the changed surface and investigate survivors. Report net tests added and removed in the PR, and identify unverified evidence.

## How it runs

For substantial work, use `generator-evaluator` with `--set 'parameters.lenses=[{name="tests"}]'` for an independent test review. Its lens reviews the writer's commit; it does not write acceptance tests. Arrange an independent acceptance author separately.

For pruning, classify tests with read-only `fan-out`, then use `big-batch` with a registered writer worktree and disjoint scope per crate. Supply the brief and required worktree or lane parameters to `provenant run --preset NAME --prompt-file BRIEF --explain` before submission.
