---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Tests pin observable invariants at public boundaries, using deterministic, modular fixtures.
Classify failures as defects (fix code), stale or rigid tests (redesign or delete), or rigid code (redesign).
When a test and the design disagree, investigate which is wrong; delete redundant tests and tests of removed mechanisms.
Merge near-duplicates into table-driven invariants; avoid exact messages, counts, order or snapshots unless they are the invariant.
Avoid duplicated declarations and implementation-coupled expectations.
Report each touched failing test as kept, redesigned, merged or deleted, with a one-line reason.

## Choose and prune

1. Invariants first: test observable behaviour and properties at public
   boundaries. Make illegal states unrepresentable in types or schemas rather
   than asserting them; name the enforcing check.
2. Add focused regression coverage where existing invariants leave a gap.
3. Prune duplicates, type-impossible cases, prose assertions and tests coupled
   to implementation. See [boundaries](references/boundaries.md) and
   [judgement](references/judgement.md) for safeguards.

For risky logic, consider mutation testing on the changed surface and investigate
survivors.
