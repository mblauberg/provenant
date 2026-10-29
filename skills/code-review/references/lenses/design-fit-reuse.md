# Lens: design fit and reuse

Canonical questions: [review-lenses.md](../review-lenses.md), section
"Ownership and structure" and its design-principle probes.

- Does the change belong where it was put? Find the canonical owner of the
  behaviour or data and compare.
- Search the repository for an existing utility, type, validation rule or flow
  that already does this. Name what should have been reused.
- Look for a second source of truth, a state machine hidden in flags, or
  coupling that crosses an ownership boundary.
- Use SOLID as probes only. Report a principle when it ties to a present defect
  or material risk, with a way to validate it.
