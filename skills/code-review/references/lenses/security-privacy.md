# Lens: security and privacy

Shared guidance: [review-lenses.md](../review-lenses.md).

- Trace untrusted input through parsing, validation, authorisation, effects,
  persistence, logging and output.
- Separate authentication from action authorisation; check ownership, role and
  confused-deputy boundaries on every mutation path.
- Check secret handling, execution and injection boundaries, and dependency
  changes. Review the unchanged enforcement the diff relies on.
- Report realistic exploits.

Provenant is built for one owner running many lanes across their own hosts,
accounts and providers. Sharing among those (notes, caches, warm targets,
co-writers in a worktree, peer graphs, relaxed or overridable checks) is the
intended design, not a finding. Report only a realistic breach of a real
boundary:
- a credential reaching a provider, a prompt, published output or another host;
- authority beyond the owner's grant;
- loss of another writer's uncommitted work;
- data loss or corruption.
Never propose new isolation, refusals or guards for a convenience path; when
risk is real but small, prefer a warning with its source shown.
