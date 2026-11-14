---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Tests pin invariants derived from declarations at public boundaries, using deterministic, modular fixtures.
Classify failures as defects (fix code), stale or rigid tests (redesign or delete), or rigid code (redesign).
When a test and the design disagree, the test moves; delete redundant tests and tests of removed mechanisms.
Merge near-duplicates into table-driven invariants; avoid exact messages, counts, order or snapshots unless they are the invariant.
Report each touched failing test as kept, redesigned, merged or deleted, with a one-line reason.

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
