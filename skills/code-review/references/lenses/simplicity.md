# Lens: simplicity

Shared guidance: [review-lenses.md](../review-lenses.md).

- Where does indirection add cost without making change easier?
- What can be deleted while keeping behaviour? Name the lines.
- Flag rigid abstractions and wrappers that merely move complexity. Consider
  foreseeable change before proposing deletion.
- Prefer a reframe that removes concepts, branches or synchronisation points,
  but only one local enough to validate. Do not demand redesign beyond the
  dependency cone.
