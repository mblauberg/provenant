---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Follow the `deliver` skill's engineering stance. Derive invariants from
declarations and test them at public boundaries with parameterised or property
tests and modular, deterministic fixtures. Do not pin exact messages, counts,
order, snapshots, pixels or geometry unless they are the invariant.

On any test failure, first decide whether the test caught a defect or pins
implementation detail: fix the code for a defect; rewrite the test as a boundary
invariant or delete it if it pins implementation. Never bend product code around
rigid tests or bump expected values to pass. Pruning is normal, welcome work:
delete redundant, duplicate, rigid, or implementation-pinned tests on touch, and
report every removed test with a one-line reason.

## Choose and prune

1. Invariants first: test observable behaviour and properties at public
   boundaries. Make illegal states unrepresentable in types or schemas rather
   than asserting them; name the enforcing check.
2. Minimal regression tests: add at most one focused check per real defect, only
   where an existing invariant cannot cover it.
3. Prune duplicates, type-impossible cases, prose assertions and tests coupled
   to implementation. See [boundaries](references/boundaries.md) and
   [judgement](references/judgement.md) for safeguards.

For risky logic, run mutation testing on the changed surface and investigate
survivors. Report net tests added and removed in the pull request. Large
pruning runs as `explore` over areas, then `batch` with disjoint write scopes.
