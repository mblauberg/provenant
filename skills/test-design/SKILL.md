---
name: test-design
description: "Use for TDD and choosing, writing, merging or deleting tests."
---

# Test design

Tests pin observable invariants at public boundaries, using deterministic,
modular fixtures. Classify a failing test as a defect (fix the code), a stale or
rigid test (redesign or delete) or rigid code (redesign); when a test and the
design disagree, investigate which is wrong. Report each touched failing test as
kept, redesigned, merged or deleted, with a one-line reason.

## Choose and prune

1. Invariants first: test observable behaviour and properties at public
   boundaries. Make illegal states unrepresentable in types or schemas rather
   than asserting them; name the enforcing check.
2. Add focused regression coverage where existing invariants leave a gap.
3. Prune duplicates, type-impossible cases, tests of removed mechanisms, prose
   assertions and tests coupled to implementation. Merge near-duplicates into
   table-driven invariants; avoid exact messages, counts, order or snapshots
   unless they are the invariant. See [boundaries](references/boundaries.md) and
   [judgement](references/judgement.md) for safeguards.
