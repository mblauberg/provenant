# Lens: security and privacy

Shared guidance: [review-lenses.md](../review-lenses.md).

- Trace untrusted input through parsing, validation, authorisation, effects,
  persistence, logging and output.
- Separate authentication from action authorisation; check ownership, role and
  confused-deputy boundaries on every mutation path.
- Check secret handling, execution and injection boundaries, and dependency
  changes. Review the unchanged enforcement the diff relies on.
- Report realistic exploits. Flag guards that block
  ordinary work without protecting a declared hard boundary.
