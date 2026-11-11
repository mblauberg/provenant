---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Choose evidence for observable behaviour and keep the suite economical. For a
bug or core logic, write the spec-derived failing test first and witness the
failure. For substantial work, an independent author derives acceptance tests
from the spec (the `review.tests` template reviews a pinned writer commit).
Deleting and trimming tests is normal work. When a test fails on your change,
decide whether it caught a defect or only pins the implementation: fix the code
in the first case, rewrite or delete the test in the second. Do not loosen an
assertion that encodes a spec clause your change breaks; raise it with the
scope owner. Report every removed or weakened test with its reason.

## Choose and prune

1. Make illegal states unrepresentable where possible, and do not test states
   that types, schemas or lint exclude; name the enforcing check.
2. One contract test per public behaviour, property tests for invariants,
   golden tests for stable wire formats, a few end-to-end tests for real
   paths. Choose the lowest boundary that proves the behaviour.
3. Search existing tests first and extend one where it fits. Budget about one
   new test per new behaviour, naming the defect only it catches.
4. On touch, delete duplicates, type-impossible cases and prose assertions;
   merge overlaps and rewrite tests coupled to implementation at the public
   boundary. A test that fails on a pure refactor needs repair.
5. Mock only process, network, clock and randomness edges; use real owned
   collaborators. See [boundaries](references/boundaries.md) and
   [judgement](references/judgement.md), whose safeguards apply whenever you
   add, keep or prune tests.

For risky logic, run mutation testing on the changed surface and investigate
survivors. Report net tests added and removed in the pull request. Large
pruning runs as `explore` over areas, then `batch` with disjoint write scopes.
