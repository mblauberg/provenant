---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Design tests extensibly, flexibly and reliably so future code evolution does not
need test updates. Put invariants and property-style checks at public boundaries
first; avoid rigid tests, example snapshots, or exact strings and counts that go
stale. Keep regression tests minimal: at most one small test per real defect,
and only where no invariant covers it.

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
3. Inspect the test on failure: if a test fails on an unbreaking change or pure
   refactor, rewrite it to pin an invariant at the public boundary or delete it.
4. Prune aggressively: delete duplicates, type-impossible cases, prose assertions
   and implementation-coupled tests. Many tests can and should be removed.
5. Mock only process, network, clock and randomness edges; use real owned
   collaborators. See [boundaries](references/boundaries.md) and
   [judgement](references/judgement.md), whose safeguards apply whenever you
   add, keep or prune tests.

For risky logic, run mutation testing on the changed surface and investigate
survivors. Report net tests added and removed in the pull request. Large
pruning runs as `explore` over areas, then `batch` with disjoint write scopes.
