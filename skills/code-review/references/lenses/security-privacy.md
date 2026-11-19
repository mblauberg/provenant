# Lens: security and privacy

Shared guidance: [review-lenses.md](../review-lenses.md).

Provenant is built for one owner running many lanes across their own hosts,
accounts and providers. Sharing among those (notes, caches, warm targets,
co-writers in a worktree, peer graphs, relaxed or overridable checks) is the
intended design, not a finding. Report only a realistic breach of a real
boundary:
- a credential or secret reaching a lane, provider, prompt, published output or
  another host that was not granted it;
- a lane moving, rewriting or deleting another lane's refs, commits or
  uncommitted files;
- authority beyond the owner's grant;
- data loss.
Never propose new isolation, refusals or guards for a convenience path; when
risk is real but small, prefer a warning with its source shown.

- Trace credentials and secrets to every sink: prompts, receipts, logs,
  published output and other hosts.
- Trace untrusted input through parsing, validation, effects and output.
- Check dependency changes.
- Report realistic exploits.
