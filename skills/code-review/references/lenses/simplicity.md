# Lens: simplicity

Canonical questions: [review-lenses.md](../review-lenses.md), section
"Ownership and structure" (simplicity, YAGNI, the 1,000-line cap).

- What can be deleted while keeping behaviour? Name the lines.
- Flag speculative or rigid abstraction, thin wrappers that only move
  complexity, options nothing uses, and layers with one caller.
- Prefer a reframe that removes concepts, branches or synchronisation points,
  but only one local enough to validate. Do not demand redesign beyond the
  dependency cone.
