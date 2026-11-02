# Lens: refactor-cleanup

Canonical questions: [review-lenses.md](../review-lenses.md), sections
"Dependency cone" and "Ownership and structure".

- Does the change address the root cause within its dependency cone?
- Identify duplicated logic the change should consolidate.
- Identify dead or obsolete paths made removable by the change.
- Check that refactoring preserves observable behaviour.
