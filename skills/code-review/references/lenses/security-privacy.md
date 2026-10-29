# Lens: security and privacy

Canonical questions: [review-lenses.md](../review-lenses.md), section
"Security and privacy overlay".

- Trace untrusted input through parsing, validation, authorisation, effects,
  persistence, logging and output.
- Separate authentication from action authorisation; check ownership, role and
  confused-deputy boundaries on every mutation path.
- Check secret handling, execution and injection boundaries, and dependency
  changes. Review the unchanged enforcement the diff relies on.
